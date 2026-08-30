use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use oven_llm::{ModelId, ProviderKind, ProviderName, ReasoningEffort, canonical_vendor};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::mcp::McpServerConfig;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("read config {0}: {1}")]
    Read(PathBuf, #[source] std::io::Error),
    #[error("parse config {0}: {1}")]
    Parse(PathBuf, #[source] toml::de::Error),
    #[error("write config {0}: {1}")]
    Write(PathBuf, #[source] std::io::Error),
    #[error("serialize config {0}: {1}")]
    Serialize(PathBuf, #[source] toml::ser::Error),
    #[error("unknown provider {0}")]
    InvalidProvider(String),
}

/// LLM provider configuration. All fields optional so users can override just
/// what they need; environment variables can supply the rest at runtime.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderConfig {
    pub name: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    /// Wire protocol for unknown vendors only. Known vendors ignore this.
    pub protocol: Option<ProviderKind>,
    pub api_key: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl ProviderConfig {
    /// Model used when neither `model` nor `OVEN_MODEL` is set.
    pub const DEFAULT_MODEL: &str = "deepseek-v4-flash";

    /// Canonicalize `name` aliases and store `model` as a wire id (no vendor).
    pub fn normalize(&mut self) {
        if let Some(name) = self.name.take() {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                self.name = None;
            } else {
                self.name = Some(canonical_vendor(trimmed));
            }
        }
        if let Some(model) = self.model.take() {
            self.model = Some(wire_model(&model));
        }
        if self.protocol.is_some() && !self.is_custom_vendor() {
            self.protocol = None;
        }
    }

    pub fn is_custom_vendor(&self) -> bool {
        match self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(name) => matches!(ProviderName::from(name), ProviderName::Custom(_)),
            None => true,
        }
    }

    /// The effective model slug: `model` config wins, then the `OVEN_MODEL` env
    /// var, then the preset for `name`, then [`ProviderConfig::DEFAULT_MODEL`].
    /// Wire ids are joined with `name` (or `deepseek` for the builtin default).
    pub fn effective_model(&self) -> String {
        if let Some(raw) = self.model.clone().or_else(|| env::var("OVEN_MODEL").ok()) {
            return qualify_model(&raw, self.name.as_deref());
        }
        if let Some(name) = self.name.as_deref()
            && let Some(suggested) = Self::suggested_model(name)
        {
            return qualify_model(suggested, Some(name));
        }
        qualify_model(Self::DEFAULT_MODEL, Some("deepseek"))
    }

    pub fn suggested_base_url(name: &str) -> Option<&'static str> {
        match canonical_vendor(name).as_str() {
            "openai" => Some("https://api.openai.com/v1"),
            "deepseek" => Some("https://api.deepseek.com"),
            "moonshot" => Some("https://api.moonshot.cn/v1"),
            "zhipu" => Some("https://open.bigmodel.cn/api/paas/v4"),
            "xai" => Some("https://api.x.ai/v1"),
            _ => None,
        }
    }

    pub fn suggested_model(name: &str) -> Option<&'static str> {
        match canonical_vendor(name).as_str() {
            "openai" => Some("gpt-5.6-terra"),
            "deepseek" => Some("deepseek-v4-flash"),
            "moonshot" => Some("kimi-k3"),
            "zhipu" => Some("glm-5.3"),
            "xai" => Some("grok-4.6"),
            _ => None,
        }
    }

    /// Fill `base_url` and `model` from [`name`](Self::name) presets.
    pub fn apply_name_presets(&mut self) {
        self.normalize();
        let Some(name) = self.name.clone() else {
            return;
        };
        if let Some(url) = Self::suggested_base_url(&name) {
            self.base_url = Some(url.to_string());
        }
        if let Some(model) = Self::suggested_model(&name) {
            self.model = Some(model.to_string());
        }
    }

    /// Provider from the canonical `name`, or the vendor segment of the model slug.
    pub fn effective_provider_name(&self) -> ProviderName {
        if let Some(name) = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return ProviderName::from(name);
        }
        match ModelId::from(self.effective_model().as_str()).vendor() {
            Some(vendor) => ProviderName::from(vendor),
            None => ProviderName::Custom("unknown".into()),
        }
    }

    pub fn parse_protocol(raw: &str) -> Option<ProviderKind> {
        match raw.to_ascii_lowercase().as_str() {
            "completions" => Some(ProviderKind::Completions),
            "responses" => Some(ProviderKind::Responses),
            "messages" => Some(ProviderKind::Messages),
            _ => None,
        }
    }

    /// The effective base URL: `base_url` config wins, then `OVEN_BASE_URL`.
    pub fn effective_base_url(&self) -> Option<String> {
        if let Some(u) = &self.base_url {
            return Some(u.clone());
        }
        env::var("OVEN_BASE_URL").ok().filter(|v| !v.is_empty())
    }

    /// The effective API key: `api_key` config wins, then `OVEN_API_KEY`.
    pub fn effective_api_key(&self) -> String {
        if let Some(k) = &self.api_key {
            return k.clone();
        }
        env::var("OVEN_API_KEY").unwrap_or_default()
    }

    /// True when no API key is configured, so interactive sessions should
    /// open `/setup` instead of failing at startup.
    pub fn needs_setup(&self) -> bool {
        self.effective_api_key().is_empty()
    }

    /// Canonical vendor slug from `name`, or the vendor segment of `model`.
    pub fn slug(&self) -> Option<String> {
        if let Some(n) = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(canonical_vendor(n));
        }
        self.model
            .as_deref()
            .and_then(|raw| ModelId::from(raw).vendor().map(canonical_vendor))
    }

    /// Overlay `Some` fields from `overlay` onto `self`.
    pub fn merge_fields(&mut self, overlay: &ProviderConfig) {
        if let Some(n) = overlay.name.clone() {
            self.name = Some(n);
        }
        if let Some(m) = overlay.model.clone() {
            self.model = Some(m);
        }
        if let Some(u) = overlay.base_url.clone() {
            self.base_url = Some(u);
        }
        if let Some(p) = overlay.protocol {
            self.protocol = Some(p);
        }
        if let Some(k) = overlay.api_key.clone() {
            self.api_key = Some(k);
        }
        if let Some(e) = overlay.reasoning_effort {
            self.reasoning_effort = Some(e);
        }
        self.normalize();
    }

    /// Copy unset fields from `src`.
    pub fn fill_missing(&mut self, src: &ProviderConfig) {
        if self.name.is_none() {
            self.name = src.name.clone();
        }
        if self.model.is_none() {
            self.model = src.model.clone();
        }
        if self.base_url.is_none() {
            self.base_url = src.base_url.clone();
        }
        if self.protocol.is_none() {
            self.protocol = src.protocol;
        }
        if self.api_key.is_none() {
            self.api_key = src.api_key.clone();
        }
        self.normalize();
    }
}

fn qualify_model(raw: &str, name: Option<&str>) -> String {
    let id = ModelId::from(raw);
    if let Some(vendor) = id.vendor() {
        id.qualify(vendor).to_string()
    } else if let Some(name) = name.map(str::trim).filter(|s| !s.is_empty()) {
        id.qualify(name).to_string()
    } else {
        raw.to_string()
    }
}

fn wire_model(raw: &str) -> String {
    let id = ModelId::from(raw);
    match id.variant() {
        Some(variant) => format!("{}:{variant}", id.wire_id()),
        None => id.wire_id().to_string(),
    }
}

/// Per-process behavioural knobs that are provider-agnostic.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AppConfig {
    #[serde(rename = "provider")]
    pub active_provider: ProviderSelection,
    /// Saved vendors keyed by canonical slug (`deepseek`, `xai`, …).
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_base_backoff_ms")]
    pub base_backoff_ms: u64,
    /// Tools to mount, by name (`file_read`, `file_write`, `bash`). Empty
    /// means the built-in default set.
    pub tools: Vec<String>,
    /// MCP server declarations. Key is the local id used to refer to a server.
    pub mcps: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderSelection {
    pub name: String,
}

#[derive(Debug, Deserialize)]
struct RawAppConfig {
    #[serde(default)]
    provider: Option<ProviderConfig>,
    #[serde(default)]
    providers: BTreeMap<String, ProviderConfig>,
    #[serde(default = "default_request_timeout_secs")]
    request_timeout_secs: u64,
    #[serde(default = "default_max_retries")]
    max_retries: u32,
    #[serde(default = "default_base_backoff_ms")]
    base_backoff_ms: u64,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    mcps: BTreeMap<String, McpServerConfig>,
}

impl<'de> Deserialize<'de> for AppConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let raw = RawAppConfig::deserialize(deserializer)?;
        let mut config = Self {
            active_provider: ProviderSelection::default(),
            providers: BTreeMap::new(),
            request_timeout_secs: raw.request_timeout_secs,
            max_retries: raw.max_retries,
            base_backoff_ms: raw.base_backoff_ms,
            tools: raw.tools,
            mcps: raw.mcps,
        };

        for (key, mut provider) in raw.providers {
            provider.normalize();
            let name = provider
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| canonical_vendor(&key));
            provider.name = Some(name.clone());
            config
                .providers
                .entry(name)
                .or_default()
                .merge_fields(&provider);
        }

        if let Some(mut provider) = raw.provider {
            provider.normalize();
            let name = provider
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| Error::custom("[provider].name is required"))?;
            let name = canonical_vendor(&name);
            provider.name = Some(name.clone());
            config
                .providers
                .entry(name.clone())
                .or_default()
                .merge_fields(&provider);
            config.active_provider.name = name;
        } else if config.providers.len() == 1 {
            config.active_provider.name = config.providers.keys().next().cloned().unwrap();
        }

        Ok(config)
    }
}

fn default_request_timeout_secs() -> u64 {
    60
}
fn default_max_retries() -> u32 {
    2
}
fn default_base_backoff_ms() -> u64 {
    500
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            active_provider: ProviderSelection {
                name: "deepseek".into(),
            },
            providers: [(
                "deepseek".into(),
                ProviderConfig {
                    name: Some("deepseek".into()),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            request_timeout_secs: default_request_timeout_secs(),
            max_retries: default_max_retries(),
            base_backoff_ms: default_base_backoff_ms(),
            tools: Vec::new(),
            mcps: BTreeMap::new(),
        }
    }
}

impl AppConfig {
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }
    pub fn base_backoff(&self) -> Duration {
        Duration::from_millis(self.base_backoff_ms)
    }

    /// Apply `overlay` on top of `self`. Non-default fields in `overlay` win.
    pub fn merge(&mut self, overlay: AppConfig) {
        if !overlay.active_provider.name.is_empty() {
            self.active_provider = overlay.active_provider;
        }
        for (name, mut provider) in overlay.providers {
            provider.normalize();
            self.providers
                .entry(canonical_vendor(&name))
                .or_default()
                .merge_fields(&provider);
        }
        if overlay.request_timeout_secs != default_request_timeout_secs() {
            self.request_timeout_secs = overlay.request_timeout_secs;
        }
        if overlay.max_retries != default_max_retries() {
            self.max_retries = overlay.max_retries;
        }
        if overlay.base_backoff_ms != default_base_backoff_ms() {
            self.base_backoff_ms = overlay.base_backoff_ms;
        }
        for name in overlay.tools {
            if !self.tools.contains(&name) {
                self.tools.push(name);
            }
        }
        for (id, cfg) in overlay.mcps {
            self.mcps.insert(id, cfg);
        }
    }

    pub fn active_provider_config(&self) -> Option<&ProviderConfig> {
        self.providers.get(&self.active_provider.name)
    }

    pub fn active_provider_config_mut(&mut self) -> Option<&mut ProviderConfig> {
        self.providers.get_mut(&self.active_provider.name)
    }

    /// True when neither the active provider nor any saved vendor has a key.
    pub fn needs_setup(&self) -> bool {
        self.providers.values().all(ProviderConfig::needs_setup)
    }

    /// Canonical slugs that already have a saved (non-empty) API key.
    pub fn configured_providers(&self) -> Vec<String> {
        self.providers
            .iter()
            .filter(|(_, provider)| !provider.needs_setup())
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn registerable_providers(&self) -> impl Iterator<Item = &ProviderConfig> {
        self.providers
            .values()
            .filter(|provider| !provider.needs_setup())
    }

    pub fn select_provider(&mut self, name: &str) -> Result<(), ConfigError> {
        let name = canonical_vendor(name);
        if !self.providers.contains_key(&name) {
            return Err(ConfigError::InvalidProvider(name));
        }
        self.active_provider.name = name;
        Ok(())
    }

    fn load_file(path: &Path) -> Result<Option<AppConfig>, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let cfg: AppConfig =
                    toml::from_str(&text).map_err(|e| ConfigError::Parse(path.to_path_buf(), e))?;
                Ok(Some(cfg))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ConfigError::Read(path.to_path_buf(), e)),
        }
    }

    /// Load configs from (user, project) files and merge them, with the
    /// project file taking precedence. Missing files are silently ignored.
    pub fn load(
        user_config: Option<&Path>,
        project_config: Option<&Path>,
    ) -> Result<Self, ConfigError> {
        let mut cfg = AppConfig::default();
        if let Some(p) = user_config
            && let Some(loaded) = Self::load_file(p)?
        {
            cfg.merge(loaded);
        }
        if let Some(p) = project_config
            && let Some(loaded) = Self::load_file(p)?
        {
            cfg.merge(loaded);
        }
        Ok(cfg)
    }

    /// Default user config location: `~/.oven/config.toml`.
    pub fn default_user_config_path() -> Option<PathBuf> {
        crate::dirs::user_config_path()
    }

    /// Default project config path: `.oven.toml` in the given workspace root.
    pub fn default_project_config_path(root: &Path) -> PathBuf {
        root.join(".oven.toml")
    }

    /// Create a template user config at the default location if it does not
    /// exist yet. Existing configs are left untouched.
    pub fn ensure_user_config() -> Result<(), ConfigError> {
        if let Some(path) = Self::default_user_config_path() {
            Self::ensure_user_config_at(&path)?;
        }
        Ok(())
    }

    fn ensure_user_config_at(path: &Path) -> Result<(), ConfigError> {
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigError::Write(path.to_path_buf(), e))?;
        }
        std::fs::write(path, DEFAULT_USER_CONFIG)
            .map_err(|e| ConfigError::Write(path.to_path_buf(), e))
    }

    pub fn save_user_provider(overlay: &ProviderConfig) -> Result<PathBuf, ConfigError> {
        let path = Self::default_user_config_path().ok_or_else(|| {
            ConfigError::Write(
                PathBuf::from("<config_home>"),
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no config dir on this platform",
                ),
            )
        })?;
        Self::save_provider_at(&path, overlay)?;
        Ok(path)
    }

    /// Update one Provider and rewrite the file in the canonical format.
    pub fn save_provider_at(path: &Path, overlay: &ProviderConfig) -> Result<(), ConfigError> {
        let mut config = Self::load_file(path)?.unwrap_or_default();
        let name = overlay
            .name
            .as_deref()
            .map(canonical_vendor)
            .filter(|name| !name.is_empty())
            .or_else(|| {
                (!config.active_provider.name.is_empty())
                    .then(|| config.active_provider.name.clone())
            })
            .ok_or_else(|| ConfigError::InvalidProvider("[provider].name is required".into()))?;
        config
            .providers
            .entry(name.clone())
            .or_default()
            .merge_fields(overlay);
        config.providers.get_mut(&name).unwrap().name = Some(name.clone());
        config.active_provider.name = name;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigError::Write(path.to_path_buf(), e))?;
        }
        let text = toml::to_string_pretty(&config)
            .map_err(|e| ConfigError::Serialize(path.to_path_buf(), e))?;
        std::fs::write(path, text).map_err(|e| ConfigError::Write(path.to_path_buf(), e))
    }
}

/// Template written to the user config location on first run. Sourced from
/// `config.example.toml` so the example and the default template stay in sync.
const DEFAULT_USER_CONFIG: &str = include_str!("../config.example.toml");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_user_config_creates_template_once() {
        let tmp = tempdir::TempDir::new("oven-config").unwrap();
        let path = tmp.path().join("config.toml");
        AppConfig::ensure_user_config_at(&path).unwrap();
        assert!(path.exists());
        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        let expected: AppConfig = toml::from_str(include_str!("../config.example.toml")).unwrap();
        assert_eq!(cfg, expected);

        std::fs::write(
            &path,
            "[provider]\nname = \"deepseek\"\nmodel = \"edited\"\n",
        )
        .unwrap();
        AppConfig::ensure_user_config_at(&path).unwrap();
        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().model.as_deref(),
            Some("edited")
        );
    }

    #[test]
    fn old_kind_field_is_ignored() {
        let cfg: AppConfig =
            toml::from_str("[provider]\nname = \"deepseek\"\nkind = \"responses\"\n").unwrap();
        assert!(cfg.active_provider_config().unwrap().protocol.is_none());

        let cfg: AppConfig =
            toml::from_str("[provider]\nname = \"deepseek\"\nkind = \"chat\"\n").unwrap();
        assert!(cfg.active_provider_config().unwrap().protocol.is_none());
    }

    #[test]
    fn old_grok_config_canonicalizes_name_and_qualifies_model() {
        let tmp = tempdir::TempDir::new("oven-old-grok").unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "[provider]\nname = \"grok\"\nmodel = \"xai/grok-4.6\"\nkind = \"responses\"\n",
        )
        .unwrap();
        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().name.as_deref(),
            Some("xai")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().model.as_deref(),
            Some("grok-4.6")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().effective_model(),
            "xai/grok-4.6"
        );
        assert!(cfg.active_provider_config().unwrap().protocol.is_none());
    }

    #[test]
    fn effective_provider_name_uses_canonical_name() {
        let cfg = ProviderConfig {
            name: Some("grok".into()),
            ..Default::default()
        };
        assert_eq!(cfg.effective_provider_name(), ProviderName::Grok);
        assert_eq!(
            ProviderConfig::default().effective_provider_name(),
            ProviderName::DeepSeek
        );
        assert_eq!(
            ProviderConfig {
                model: Some("plain-id".into()),
                ..Default::default()
            }
            .effective_provider_name(),
            ProviderName::Custom("unknown".into())
        );
        assert_eq!(ProviderName::from("kimi"), ProviderName::Moonshot);
        assert_eq!(
            ProviderName::from("my-gateway"),
            ProviderName::Custom("my-gateway".into())
        );
    }

    #[test]
    fn effective_model_falls_back_to_default() {
        let cfg = ProviderConfig::default();
        assert_eq!(cfg.effective_model(), "deepseek/deepseek-v4-flash");

        let cfg = ProviderConfig {
            model: Some("deepseek-v4-flash".into()),
            name: Some("deepseek".into()),
            ..Default::default()
        };
        assert_eq!(cfg.effective_model(), "deepseek/deepseek-v4-flash");
    }

    #[test]
    fn configured_credentials_win_over_env() {
        let cfg = ProviderConfig {
            base_url: Some("https://proxy.example".into()),
            api_key: Some("sk-configured".into()),
            ..Default::default()
        };
        assert_eq!(
            cfg.effective_base_url().as_deref(),
            Some("https://proxy.example")
        );
        assert_eq!(cfg.effective_api_key(), "sk-configured");
        assert!(!cfg.needs_setup());
    }

    #[test]
    fn name_presets_base_url_and_model() {
        let mut cfg = ProviderConfig {
            name: Some("moonshot".into()),
            ..Default::default()
        };
        cfg.apply_name_presets();
        assert_eq!(cfg.base_url.as_deref(), Some("https://api.moonshot.cn/v1"));
        assert_eq!(cfg.model.as_deref(), Some("kimi-k3"));
        assert_eq!(cfg.effective_model(), "moonshot/kimi-k3");
        assert_eq!(ProviderConfig::suggested_model("grok"), Some("grok-4.6"));
        assert_eq!(
            ProviderConfig {
                name: Some("zhipu".into()),
                ..Default::default()
            }
            .effective_model(),
            "zhipu/glm-5.3"
        );
        let mut grok = ProviderConfig {
            name: Some("grok".into()),
            ..Default::default()
        };
        grok.apply_name_presets();
        assert_eq!(grok.name.as_deref(), Some("xai"));
        assert_eq!(grok.model.as_deref(), Some("grok-4.6"));
        assert_eq!(grok.effective_model(), "xai/grok-4.6");
    }

    #[test]
    fn save_provider_at_merges_only_set_fields() {
        let tmp = tempdir::TempDir::new("oven-save-provider").unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "max_retries = 9\n\n[provider]\nname = \"proxy\"\nmodel = \"old\"\n",
        )
        .unwrap();

        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("proxy".into()),
                protocol: Some(ProviderKind::Responses),
                base_url: Some("https://proxy.example".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(cfg.max_retries, 9);
        assert_eq!(
            cfg.active_provider_config().unwrap().model.as_deref(),
            Some("old")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().protocol,
            Some(ProviderKind::Responses)
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().base_url.as_deref(),
            Some("https://proxy.example")
        );
        assert!(cfg.active_provider_config().unwrap().api_key.is_none());
        let text = std::fs::read_to_string(&path).unwrap();
        let retries_at = text.find("max_retries").expect("root max_retries");
        let table_at = text.find("[provider]").expect("[provider]");
        assert!(
            retries_at < table_at,
            "root keys must stay above [provider]: {text}"
        );
    }

    #[test]
    fn save_provider_at_repairs_keys_swallowed_by_provider_table() {
        let tmp = tempdir::TempDir::new("oven-save-provider-repair").unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "[provider]\nname = \"deepseek\"\nmodel = \"old\"\n").unwrap();

        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("moonshot".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(cfg.max_retries, 2);
        assert_eq!(cfg.request_timeout_secs, 60);
        assert_eq!(cfg.active_provider_config().unwrap().model.as_deref(), None);
        assert_eq!(
            cfg.active_provider_config().unwrap().effective_model(),
            "moonshot/kimi-k3"
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().name.as_deref(),
            Some("moonshot")
        );
        assert!(text.find("max_retries").unwrap() < text.find("[provider]").unwrap());
    }

    #[test]
    fn save_provider_at_writes_reasoning_effort() {
        let tmp = tempdir::TempDir::new("oven-save-effort").unwrap();
        let path = tmp.path().join("config.toml");
        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("openai".into()),
                model: Some("gpt-4o".into()),
                reasoning_effort: Some(ReasoningEffort::Medium),
                ..Default::default()
            },
        )
        .unwrap();

        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().model.as_deref(),
            Some("gpt-4o")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().reasoning_effort,
            Some(ReasoningEffort::Medium)
        );
    }

    #[test]
    fn load_migrates_legacy_provider_into_map() {
        let tmp = tempdir::TempDir::new("oven-hydrate-legacy").unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "[provider]\nname = \"deepseek\"\napi_key = \"sk-old\"\nmodel = \"deepseek-v4-flash\"\n",
        )
        .unwrap();
        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().name.as_deref(),
            Some("deepseek")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().api_key.as_deref(),
            Some("sk-old")
        );
        let saved = cfg.providers.get("deepseek").expect("hydrated");
        assert_eq!(saved.api_key.as_deref(), Some("sk-old"));
        assert_eq!(saved.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(cfg.configured_providers(), vec!["deepseek"]);
        assert!(!cfg.needs_setup());
    }

    #[test]
    fn save_second_vendor_keeps_first() {
        let tmp = tempdir::TempDir::new("oven-save-two").unwrap();
        let path = tmp.path().join("config.toml");
        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("deepseek".into()),
                api_key: Some("sk-ds".into()),
                model: Some("deepseek-v4-flash".into()),
                ..Default::default()
            },
        )
        .unwrap();
        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("xai".into()),
                api_key: Some("xai-key".into()),
                model: Some("grok-4.6".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().name.as_deref(),
            Some("xai")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().model.as_deref(),
            Some("grok-4.6")
        );
        assert_eq!(cfg.providers["deepseek"].api_key.as_deref(), Some("sk-ds"));
        assert_eq!(cfg.providers["xai"].api_key.as_deref(), Some("xai-key"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[providers.deepseek]"));
        assert!(text.contains("[providers.xai]"));
    }

    #[test]
    fn save_model_updates_active_and_saved_model_not_api_key() {
        let tmp = tempdir::TempDir::new("oven-save-model").unwrap();
        let path = tmp.path().join("config.toml");
        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("deepseek".into()),
                api_key: Some("sk-ds".into()),
                model: Some("deepseek-v4-flash".into()),
                ..Default::default()
            },
        )
        .unwrap();
        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("deepseek".into()),
                model: Some("deepseek-chat".into()),
                reasoning_effort: Some(ReasoningEffort::High),
                ..Default::default()
            },
        )
        .unwrap();

        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().model.as_deref(),
            Some("deepseek-chat")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().reasoning_effort,
            Some(ReasoningEffort::High)
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().api_key.as_deref(),
            Some("sk-ds")
        );
        assert_eq!(cfg.providers["deepseek"].api_key.as_deref(), Some("sk-ds"));
        assert_eq!(
            cfg.providers["deepseek"].model.as_deref(),
            Some("deepseek-chat")
        );
    }

    #[test]
    fn load_selects_active_provider_from_map() {
        let tmp = tempdir::TempDir::new("oven-hydrate-map").unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "[provider]\nname = \"xai\"\nmodel = \"grok-4.6\"\n\n[providers.xai]\napi_key = \"xai-key\"\n[providers.deepseek]\napi_key = \"sk-ds\"\nmodel = \"deepseek-v4-flash\"\n",
        )
        .unwrap();
        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().api_key.as_deref(),
            Some("xai-key")
        );
        assert_eq!(cfg.providers["deepseek"].api_key.as_deref(), Some("sk-ds"));
        assert_eq!(cfg.configured_providers(), vec!["deepseek", "xai"]);
    }
}

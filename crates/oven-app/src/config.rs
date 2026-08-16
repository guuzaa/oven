use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use oven_llm::{ProviderKind, ProviderName, ReasoningEffort};
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
}

/// LLM provider configuration. All fields optional so users can override just
/// what they need; environment variables can supply the rest at runtime.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderConfig {
    pub name: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub kind: Option<ProviderKind>,
    pub api_key: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl ProviderConfig {
    /// The effective API kind, defaulting to chat completions.
    pub fn effective_kind(&self) -> ProviderKind {
        self.kind.unwrap_or(ProviderKind::Completions)
    }

    /// Model used when neither `model` nor `OVEN_MODEL` is set.
    pub const DEFAULT_MODEL: &str = "deepseek-v4-flash";

    /// The effective model: `model` config wins, then the `OVEN_MODEL` env
    /// var, then the preset for `name`, then [`ProviderConfig::DEFAULT_MODEL`].
    pub fn effective_model(&self) -> String {
        self.model
            .clone()
            .or_else(|| env::var("OVEN_MODEL").ok())
            .or_else(|| {
                self.name
                    .as_deref()
                    .and_then(Self::suggested_model)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| Self::DEFAULT_MODEL.to_string())
    }

    pub fn suggested_base_url(name: &str) -> Option<&'static str> {
        match name.to_ascii_lowercase().as_str() {
            "openai" => Some("https://api.openai.com/v1"),
            "deepseek" => Some("https://api.deepseek.com"),
            "moonshot" | "kimi" => Some("https://api.moonshot.cn/v1"),
            "zhipu" | "glm" => Some("https://open.bigmodel.cn/api/paas/v4"),
            "grok" => Some("https://api.x.ai/v1"),
            _ => None,
        }
    }

    pub fn suggested_model(name: &str) -> Option<&'static str> {
        match name.to_ascii_lowercase().as_str() {
            "openai" => Some("gpt-5.6-terra"),
            "deepseek" => Some("deepseek-v4-flash"),
            "moonshot" | "kimi" => Some("kimi-k3"),
            "zhipu" | "glm" => Some("glm-5.3"),
            "grok" => Some("grok-4.6"),
            _ => None,
        }
    }

    /// Fill `base_url` and `model` from [`name`](Self::name) presets.
    pub fn apply_name_presets(&mut self) {
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

    /// Provider from an explicit `name` when set, otherwise inferred from the
    /// model prefix. Unrecognized prefixes map to `Custom("unknown")`; callers
    /// may still supply a `base_url` to make such models usable through an
    /// OpenAI-compatible endpoint.
    pub fn effective_provider_name(&self, model: &str) -> ProviderName {
        match self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(name) => ProviderName::from(name),
            None => Self::provider_name_for(model),
        }
    }

    pub fn parse_kind(raw: &str) -> Option<ProviderKind> {
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

    /// Single source of truth for model-prefix → provider routing. Used by
    /// [`ProviderConfig::effective_provider_name`] and the env-var lookups so
    /// a model never resolves to two different providers.
    fn provider_name_for(model: &str) -> ProviderName {
        let lower = model.to_ascii_lowercase();
        if lower.starts_with("claude") {
            ProviderName::Anthropic
        } else if lower.starts_with("gpt") {
            ProviderName::OpenAI
        } else if lower.starts_with("deepseek") {
            ProviderName::DeepSeek
        } else if lower.starts_with("kimi") {
            ProviderName::Moonshot
        } else if lower.starts_with("glm") {
            ProviderName::Zhipu
        } else {
            ProviderName::Custom("unknown".into())
        }
    }
}

/// Per-process behavioural knobs that are provider-agnostic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    #[serde(default)]
    pub provider: ProviderConfig,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_base_backoff_ms")]
    pub base_backoff_ms: u64,
    /// Tools to mount, by name (`file_read`, `file_write`, `bash`). Empty
    /// means the built-in default set.
    #[serde(default)]
    pub tools: Vec<String>,
    /// MCP server declarations. Key is the local id used to refer to a server.
    #[serde(default)]
    pub mcps: BTreeMap<String, McpServerConfig>,
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
            provider: ProviderConfig::default(),
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
        if let Some(n) = overlay.provider.name {
            self.provider.name = Some(n);
        }
        if let Some(m) = overlay.provider.model {
            self.provider.model = Some(m);
        }
        if let Some(u) = overlay.provider.base_url {
            self.provider.base_url = Some(u);
        }
        if let Some(e) = overlay.provider.kind {
            self.provider.kind = Some(e);
        }
        if let Some(k) = overlay.provider.api_key {
            self.provider.api_key = Some(k);
        }
        if let Some(e) = overlay.provider.reasoning_effort {
            self.provider.reasoning_effort = Some(e);
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
        // Lists/maps: union-with-overlay-wins per key/id.
        for name in overlay.tools {
            if !self.tools.contains(&name) {
                self.tools.push(name);
            }
        }
        for (id, cfg) in overlay.mcps {
            self.mcps.insert(id, cfg);
        }
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
            cfg = loaded;
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

    /// Merge `overlay` into the user config file at the default location.
    /// Only `Some` fields are written; existing keys are otherwise left as-is.
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

    /// Merge `overlay` into the `[provider]` table at `path`. Missing files
    /// are created. Only `Some` fields overwrite existing keys.
    pub fn save_provider_at(path: &Path, overlay: &ProviderConfig) -> Result<(), ConfigError> {
        let mut root = match std::fs::read_to_string(path) {
            Ok(text) if !text.trim().is_empty() => toml::from_str::<toml::Table>(&text)
                .map_err(|e| ConfigError::Parse(path.to_path_buf(), e))?,
            Ok(_) => toml::Table::new(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(e) => return Err(ConfigError::Read(path.to_path_buf(), e)),
        };
        let mut provider = match root.remove("provider") {
            Some(toml::Value::Table(table)) => table,
            _ => toml::Table::new(),
        };
        hoist_root_keys(&mut provider, &mut root);
        if let Some(n) = &overlay.name {
            provider.insert("name".into(), toml::Value::String(n.clone()));
        }
        if let Some(m) = &overlay.model {
            provider.insert("model".into(), toml::Value::String(m.clone()));
        }
        if let Some(u) = &overlay.base_url {
            provider.insert("base_url".into(), toml::Value::String(u.clone()));
        }
        if let Some(k) = overlay.kind {
            provider.insert("kind".into(), toml::Value::String(k.to_string()));
        }
        if let Some(k) = &overlay.api_key {
            provider.insert("api_key".into(), toml::Value::String(k.clone()));
        }
        if let Some(e) = overlay.reasoning_effort {
            provider.insert(
                "reasoning_effort".into(),
                toml::Value::String(e.to_string()),
            );
        }
        root.insert("provider".into(), toml::Value::Table(provider));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigError::Write(path.to_path_buf(), e))?;
        }
        let text = emit_toml(&root).map_err(|e| ConfigError::Serialize(path.to_path_buf(), e))?;
        std::fs::write(path, text).map_err(|e| ConfigError::Write(path.to_path_buf(), e))
    }
}

const ROOT_KEYS: &[&str] = &[
    "request_timeout_secs",
    "max_retries",
    "base_backoff_ms",
    "tools",
    "mcps",
];

fn hoist_root_keys(provider: &mut toml::Table, root: &mut toml::Table) {
    for key in ROOT_KEYS {
        if let Some(value) = provider.remove(*key)
            && !root.contains_key(*key)
        {
            root.insert((*key).into(), value);
        }
    }
}

fn emit_toml(root: &toml::Table) -> Result<String, toml::ser::Error> {
    let mut scalars = toml::Table::new();
    let mut tables = toml::Table::new();
    for (key, value) in root {
        if matches!(value, toml::Value::Table(_)) {
            tables.insert(key.clone(), value.clone());
        } else {
            scalars.insert(key.clone(), value.clone());
        }
    }
    let mut out = String::new();
    if !scalars.is_empty() {
        out.push_str(&toml::to_string_pretty(&scalars)?);
    }
    if !tables.is_empty() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&toml::to_string_pretty(&tables)?);
    }
    Ok(out)
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

        std::fs::write(&path, "[provider]\nmodel = \"edited\"\n").unwrap();
        AppConfig::ensure_user_config_at(&path).unwrap();
        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(cfg.provider.model.as_deref(), Some("edited"));
    }

    #[test]
    fn kind_defaults_to_completions_and_merges() {
        let cfg: AppConfig = toml::from_str("[provider]\nmodel = \"m\"\n").unwrap();
        assert_eq!(cfg.provider.effective_kind(), ProviderKind::Completions);

        let mut cfg: AppConfig = toml::from_str("[provider]\nkind = \"responses\"\n").unwrap();
        assert_eq!(cfg.provider.effective_kind(), ProviderKind::Responses);

        // An overlay without entrypoint must not override an explicit value.
        cfg.merge(AppConfig::default());
        assert_eq!(cfg.provider.effective_kind(), ProviderKind::Responses);
    }

    #[test]
    fn kind_rejects_unknown_values() {
        let err = toml::from_str::<AppConfig>("[provider]\nkind = \"chat\"\n").unwrap_err();
        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn provider_name_routes_model_prefixes() {
        let cfg = ProviderConfig::default();
        let cases = [
            ("gpt-4o", ProviderName::OpenAI),
            ("gpt-o3-mini", ProviderName::OpenAI),
            ("deepseek-chat", ProviderName::DeepSeek),
            ("kimi-k3", ProviderName::Moonshot),
            ("glm-4", ProviderName::Zhipu),
            ("claude-3-5-haiku", ProviderName::Anthropic),
            ("unknown-model", ProviderName::Custom("unknown".into())),
        ];
        for (model, expected) in cases {
            assert_eq!(cfg.effective_provider_name(model), expected, "{model}");
        }
    }

    #[test]
    fn effective_model_falls_back_to_default() {
        let cfg = ProviderConfig::default();
        assert_eq!(cfg.effective_model(), ProviderConfig::DEFAULT_MODEL);

        let cfg = ProviderConfig {
            model: Some("deepseek-v4-flash".into()),
            ..Default::default()
        };
        assert_eq!(cfg.effective_model(), "deepseek-v4-flash");
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
        assert_eq!(ProviderConfig::suggested_model("grok"), Some("grok-4.6"));
        assert_eq!(
            ProviderConfig {
                name: Some("zhipu".into()),
                ..Default::default()
            }
            .effective_model(),
            "glm-5.3"
        );
    }

    #[test]
    fn explicit_name_wins_over_model_prefix() {
        let cfg = ProviderConfig {
            name: Some("openai".into()),
            ..Default::default()
        };
        assert_eq!(
            cfg.effective_provider_name("claude-3-5-haiku"),
            ProviderName::OpenAI
        );
        assert_eq!(ProviderName::from("kimi"), ProviderName::Moonshot);
        assert_eq!(
            ProviderName::from("my-gateway"),
            ProviderName::Custom("my-gateway".into())
        );
    }

    #[test]
    fn save_provider_at_merges_only_set_fields() {
        let tmp = tempdir::TempDir::new("oven-save-provider").unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "max_retries = 9\n\n[provider]\nmodel = \"old\"\n").unwrap();

        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                kind: Some(ProviderKind::Responses),
                base_url: Some("https://proxy.example".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(cfg.max_retries, 9);
        assert_eq!(cfg.provider.model.as_deref(), Some("old"));
        assert_eq!(cfg.provider.effective_kind(), ProviderKind::Responses);
        assert_eq!(
            cfg.provider.base_url.as_deref(),
            Some("https://proxy.example")
        );
        assert!(cfg.provider.api_key.is_none());
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
        std::fs::write(
            &path,
            "[provider]\nmodel = \"old\"\nmax_retries = 9\nrequest_timeout_secs = 30\n",
        )
        .unwrap();

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
        assert_eq!(cfg.max_retries, 9);
        assert_eq!(cfg.request_timeout_secs, 30);
        assert_eq!(cfg.provider.model.as_deref(), Some("old"));
        assert_eq!(cfg.provider.name.as_deref(), Some("moonshot"));
        assert!(text.find("max_retries").unwrap() < text.find("[provider]").unwrap());
    }

    #[test]
    fn save_provider_at_writes_reasoning_effort() {
        let tmp = tempdir::TempDir::new("oven-save-effort").unwrap();
        let path = tmp.path().join("config.toml");
        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                model: Some("gpt-4o".into()),
                reasoning_effort: Some(ReasoningEffort::Medium),
                ..Default::default()
            },
        )
        .unwrap();

        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(cfg.provider.model.as_deref(), Some("gpt-4o"));
        assert_eq!(cfg.provider.reasoning_effort, Some(ReasoningEffort::Medium));
    }
}

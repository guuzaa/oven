use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use oven_llm::ProviderKind;
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
}

impl ProviderConfig {
    /// The effective API kind, defaulting to chat completions.
    pub fn effective_kind(&self) -> ProviderKind {
        self.kind.unwrap_or(ProviderKind::Completions)
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

    /// Default user config location: `$XDG_CONFIG_HOME/oven/config.toml`
    /// (or `~/.config/oven/config.toml`).
    pub fn default_user_config_path() -> Option<PathBuf> {
        cross_xdg::BaseDirs::with_prefix("oven")
            .ok()
            .map(|d| d.config_home().join("config.toml"))
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
}

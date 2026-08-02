use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::mcp::McpServerConfig;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("read config {0}: {1}")]
    Read(PathBuf, #[source] std::io::Error),
    #[error("parse config {0}: {1}")]
    Parse(PathBuf, #[source] serde_yaml::Error),
}

/// LLM provider configuration. All fields optional so users can override just
/// what they need; environment variables can supply the rest at runtime.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderConfig {
    pub model: Option<String>,
    pub base_url: Option<String>,
    /// Override of the API key. When unset, the bundled adapter's default env
    /// var is consulted (e.g. `ANTHROPIC_API_KEY`).
    pub api_key: Option<String>,
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
    /// Skills to enable, by id. Entries that match no registered skill are
    /// silently skipped.
    #[serde(default)]
    pub skills: Vec<String>,
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
            skills: Vec::new(),
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
        if let Some(m) = overlay.provider.model {
            self.provider.model = Some(m);
        }
        if let Some(u) = overlay.provider.base_url {
            self.provider.base_url = Some(u);
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
        for id in overlay.skills {
            if !self.skills.contains(&id) {
                self.skills.push(id);
            }
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

    fn load_file(path: &Path) -> Result<Option<AppConfig>, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let cfg: AppConfig = serde_yaml::from_str(&text)
                    .map_err(|e| ConfigError::Parse(path.to_path_buf(), e))?;
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

    /// Default user config location: `$HOME/.config/oven/config.yaml`.
    pub fn default_user_config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("oven").join("config.yaml"))
    }

    /// Default project config path: `.oven.yaml` in the given workspace root.
    pub fn default_project_config_path(root: &Path) -> PathBuf {
        root.join(".oven.yaml")
    }
}

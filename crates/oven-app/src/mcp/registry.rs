//! MCP server registration.
//!
//! The wire protocol itself lives in [`crate::mcp::client`]; this module only
//! declares which MCP servers the user wants enabled and keeps them in an
//! in-memory registry.
//!
//! Config shape (in `.oven.toml`):
//!
//! ```toml
//! [mcps.filesystem]
//! command = "npx"
//! args = ["-y", "@modelcontextprotocol/server-filesystem", "/abs/path"]
//!
//! [mcps.filesystem.env]
//! FOO = "bar"
//!
//! Alternatively point at a streamable HTTP endpoint (headers optional):
//!
//! ```toml
//! [mcps.remote]
//! url = "https://example.com/mcp"
//!
//! [mcps.remote.headers]
//! Authorization = "Bearer sk-xxx"
//! ```
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct McpServerConfig {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Streamable HTTP endpoint. When set, `command`/`args`/`env` are ignored.
    #[serde(default)]
    pub url: Option<String>,
    /// Extra headers for HTTP servers (e.g. `Authorization`).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Error)]
pub enum McpError {
    #[error("duplicate mcp id: {0}")]
    Duplicate(String),
    #[error("unknown mcp id: {0}")]
    Unknown(String),
    #[error("empty mcp config for '{0}' (need command or url)")]
    EmptyConfig(String),
}

/// In-memory registry of declared MCP servers. Built from `AppConfig::mcps`.
#[derive(Default)]
pub struct McpRegistry {
    servers: BTreeMap<String, McpServerConfig>,
}

impl McpRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        id: impl Into<String>,
        cfg: McpServerConfig,
    ) -> Result<(), McpError> {
        let id: String = id.into();
        let has_url = cfg.url.as_deref().is_some_and(|u| !u.trim().is_empty());
        if !has_url && cfg.command.trim().is_empty() {
            return Err(McpError::EmptyConfig(id));
        }
        if self.servers.contains_key(&id) {
            return Err(McpError::Duplicate(id));
        }
        self.servers.insert(id, cfg);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<&McpServerConfig, McpError> {
        self.servers
            .get(id)
            .ok_or_else(|| McpError::Unknown(id.to_string()))
    }

    pub fn ids(&self) -> Vec<&str> {
        self.servers.keys().map(|s| s.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.servers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &McpServerConfig)> {
        self.servers.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(cmd: &str) -> McpServerConfig {
        McpServerConfig {
            command: cmd.to_string(),
            args: vec!["--foo".into()],
            env: BTreeMap::from([("KEY".into(), "val".into())]),
            ..Default::default()
        }
    }

    #[test]
    fn register_and_lookup() {
        let mut reg = McpRegistry::new();
        reg.register("fs", cfg("npx")).unwrap();
        assert_eq!(reg.len(), 1);
        let got = reg.get("fs").unwrap();
        assert_eq!(got.command, "npx");
        assert_eq!(got.args, vec!["--foo".to_string()]);
    }

    #[test]
    fn duplicate_id_rejected() {
        let mut reg = McpRegistry::new();
        reg.register("fs", cfg("npx")).unwrap();
        let err = reg.register("fs", cfg("npx")).unwrap_err();
        assert!(matches!(err, McpError::Duplicate(_)));
    }

    #[test]
    fn empty_config_rejected() {
        let mut reg = McpRegistry::new();
        let err = reg.register("fs", McpServerConfig::default()).unwrap_err();
        assert!(matches!(err, McpError::EmptyConfig(_)));
    }

    #[test]
    fn url_only_config_accepted() {
        let mut reg = McpRegistry::new();
        reg.register(
            "http",
            McpServerConfig {
                command: String::new(),
                url: Some("https://example.com/mcp".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn unknown_id_lookup_errors() {
        let reg = McpRegistry::new();
        assert!(matches!(reg.get("x"), Err(McpError::Unknown(_))));
    }
}

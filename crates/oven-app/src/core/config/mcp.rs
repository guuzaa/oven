//! `[mcps.<id>]` config shape: which MCP servers the user declared. The
//! registry that keeps them alive lives in the capabilities layer.

use std::collections::BTreeMap;
use std::string::String;

use serde::{Deserialize, Serialize};

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

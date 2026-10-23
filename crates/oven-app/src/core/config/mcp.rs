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

/// How a server is reached: a spawned process or an HTTP endpoint. The two
/// use disjoint fields, so a server that switches between them replaces the
/// other one's fields entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Stdio,
    Http,
}

impl McpServerConfig {
    fn transport(&self) -> Transport {
        match self.url.as_deref() {
            Some(url) if !url.trim().is_empty() => Transport::Http,
            _ => Transport::Stdio,
        }
    }

    /// Overlay `incoming` on this one: the incoming fields win, the ones it
    /// leaves unset keep what was stored. A server that changes transport
    /// replaces the other transport's fields, which a field-wise merge
    /// cannot express.
    pub fn merge_fields(&mut self, incoming: &McpServerConfig) {
        if incoming.transport() != self.transport() {
            *self = incoming.clone();
            return;
        }
        if !incoming.command.is_empty() {
            self.command.clone_from(&incoming.command);
        }
        if !incoming.args.is_empty() {
            self.args.clone_from(&incoming.args);
        }
        if incoming.url.is_some() {
            self.url.clone_from(&incoming.url);
        }
        for (key, value) in &incoming.env {
            self.env.insert(key.clone(), value.clone());
        }
        for (key, value) in &incoming.headers {
            self.headers.insert(key.clone(), value.clone());
        }
    }
}

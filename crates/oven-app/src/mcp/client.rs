//! Client-side MCP over stdio via rmcp: spawn each declared server, negotiate
//! the protocol, list its tools, and bridge them into the agent as [`Tool`]s.
//!
//! The protocol work sits behind two small traits so tests can mock the
//! server side with mockall instead of spawning a real process:
//! [`McpCaller`] covers a single `tools/call`, [`McpConnector`] covers
//! connecting a configured server and listing its tools.

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use http::{HeaderName, HeaderValue};
use oven_agent::{AgentError, CancellationToken, Tool};
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, ResourceContents};
use rmcp::service::{RoleClient, RunningService, serve_client};
use rmcp::transport::TokioChildProcess;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use serde_json::Value;
use tokio::process::Command;

use super::registry::{McpRegistry, McpServerConfig};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_TOOL_NAME: usize = 64;

/// One `tools/call` on a connected MCP server.
#[async_trait]
pub trait McpCaller: Send + Sync {
    async fn call_tool(&self, params: CallToolRequestParams) -> Result<CallToolResult, String>;
}

/// Connects the configured MCP servers and returns their bridged tools.
#[async_trait]
pub trait McpConnector: Send + Sync {
    async fn connect(&self, registry: &McpRegistry, root: &Path) -> Result<Vec<McpTool>, String>;
}

/// rmcp-backed [`McpCaller`] over an established stdio session.
struct RmcpCaller {
    client: Arc<RunningService<RoleClient, ()>>,
}

#[async_trait]
impl McpCaller for RmcpCaller {
    async fn call_tool(&self, params: CallToolRequestParams) -> Result<CallToolResult, String> {
        self.client
            .call_tool(params)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Default connector: connects every server by its config — stdio via a
/// spawned child process, HTTP via the server's streamable HTTP endpoint.
/// Fails fast on the first server that cannot be connected or negotiated.
#[derive(Default)]
pub struct DefaultMcpConnector;

#[async_trait]
impl McpConnector for DefaultMcpConnector {
    async fn connect(&self, registry: &McpRegistry, root: &Path) -> Result<Vec<McpTool>, String> {
        let mut tools = vec![];
        for (id, cfg) in registry.iter() {
            let has_url = cfg.url.as_deref().is_some_and(|u| !u.trim().is_empty());
            let connected = if has_url {
                connect_http(id, cfg).await
            } else {
                connect_stdio(id, cfg, root).await
            }
            .map_err(|e| format!("mcp '{id}': {e}"))?;
            tools.extend(connected);
        }
        Ok(tools)
    }
}

/// A tool hosted by a remote MCP server, exposed to the model under
/// `<server_id>_<tool_name>` so tools from different servers cannot collide.
pub struct McpTool {
    server_id: String,
    remote_name: String,
    name: String,
    description: String,
    schema: Value,
    caller: Arc<dyn McpCaller>,
}

impl McpTool {
    pub fn new(
        server_id: impl Into<String>,
        remote_name: impl Into<String>,
        description: impl Into<String>,
        schema: Value,
        caller: Arc<dyn McpCaller>,
    ) -> Self {
        let server_id = server_id.into();
        let remote_name = remote_name.into();
        let name = mcp_tool_name(&server_id, &remote_name);
        let description = format!("[mcp:{server_id}] {}", description.into())
            .trim()
            .to_string();
        Self {
            server_id,
            remote_name,
            name,
            description,
            schema,
            caller,
        }
    }
}

async fn connect_stdio(
    server_id: &str,
    cfg: &McpServerConfig,
    root: &Path,
) -> Result<Vec<McpTool>, String> {
    let mut cmd = Command::new(&cfg.command);
    cmd.args(&cfg.args);
    for (key, value) in &cfg.env {
        cmd.env(key, value);
    }
    cmd.current_dir(root);

    let transport = TokioChildProcess::new(cmd).map_err(|e| format!("spawn: {e}"))?;
    let client = tokio::time::timeout(CONNECT_TIMEOUT, serve_client((), transport))
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| format!("handshake: {e}"))?;
    collect_tools(server_id, Arc::new(client)).await
}

async fn connect_http(server_id: &str, cfg: &McpServerConfig) -> Result<Vec<McpTool>, String> {
    let url = cfg
        .url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or_else(|| "missing url".to_string())?;

    let mut config = StreamableHttpClientTransportConfig::with_uri(url);
    if !cfg.headers.is_empty() {
        let mut headers = std::collections::HashMap::new();
        for (name, value) in &cfg.headers {
            let name =
                HeaderName::from_str(name).map_err(|e| format!("invalid header '{name}': {e}"))?;
            let value = HeaderValue::from_str(value)
                .map_err(|e| format!("invalid header value for '{name}': {e}"))?;
            headers.insert(name, value);
        }
        config = config.custom_headers(headers);
    }

    let transport = StreamableHttpClientTransport::from_config(config);
    let client = tokio::time::timeout(CONNECT_TIMEOUT, serve_client((), transport))
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| format!("handshake: {e}"))?;
    collect_tools(server_id, Arc::new(client)).await
}

async fn collect_tools(
    server_id: &str,
    client: Arc<RunningService<RoleClient, ()>>,
) -> Result<Vec<McpTool>, String> {
    let remote_tools = tokio::time::timeout(CONNECT_TIMEOUT, client.peer().list_all_tools())
        .await
        .map_err(|_| "list_tools timed out".to_string())?
        .map_err(|e| format!("list_tools: {e}"))?;

    let mut tools = Vec::with_capacity(remote_tools.len());
    for remote in remote_tools {
        let remote_name = remote.name.clone().into_owned();
        let schema = remote.schema_as_json_value();
        let description = remote
            .description
            .as_deref()
            .map(str::to_string)
            .unwrap_or_default();
        tools.push(McpTool::new(
            server_id,
            remote_name,
            description,
            schema,
            Arc::new(RmcpCaller {
                client: client.clone(),
            }),
        ));
    }
    Ok(tools)
}

/// `<server_id>_<tool_name>` with non-alphanumerics replaced by `_` and a
/// hard cap so the result stays a valid, short provider tool id.
fn mcp_tool_name(server_id: &str, tool_name: &str) -> String {
    let id: String = server_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut name = format!("{id}_{tool_name}");
    let end = name.floor_char_boundary(MAX_TOOL_NAME);
    name.truncate(end);
    name
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    async fn run(
        &self,
        args: &Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError> {
        let arguments = args.as_object().cloned().ok_or_else(|| {
            AgentError::from(format!(
                "mcp:{}: arguments must be an object",
                self.server_id
            ))
        })?;
        let request =
            CallToolRequestParams::new(self.remote_name.clone()).with_arguments(arguments);

        let result = if let Some(cancel) = cancel {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(AgentError::cancelled()),
                res = self.caller.call_tool(request) => res,
            }
        } else {
            self.caller.call_tool(request).await
        };
        let result = result.map_err(|e| {
            AgentError::from(format!(
                "mcp:{} tool {}: {}",
                self.server_id, self.remote_name, e
            ))
        })?;
        Ok(format_result(&result))
    }
}

/// Render an MCP tool result to plain text, summarizing non-text blocks.
fn format_result(result: &CallToolResult) -> String {
    let mut text = String::new();
    for block in &result.content {
        if !text.is_empty() {
            text.push('\n');
        }
        match block {
            ContentBlock::Text(t) => text.push_str(&t.text),
            ContentBlock::Image(img) => {
                text.push_str(&format!("[image: {}]", img.mime_type));
            }
            ContentBlock::Audio(a) => {
                text.push_str(&format!("[audio: {}]", a.mime_type));
            }
            ContentBlock::Resource(r) => {
                let uri = match &r.resource {
                    ResourceContents::TextResourceContents { uri, .. }
                    | ResourceContents::BlobResourceContents { uri, .. } => uri.as_str(),
                    _ => "(unknown)",
                };
                text.push_str(&format!("[resource: {uri}]"));
            }
            ContentBlock::ResourceLink(_) => text.push_str("[resource_link]"),
            _ => text.push_str("[content]"),
        }
    }
    if let Some(structured) = &result.structured_content {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(
            &serde_json::to_string_pretty(structured).unwrap_or_else(|_| structured.to_string()),
        );
    }
    if text.is_empty() {
        text.push_str("(no output)");
    }
    if result.is_error == Some(true) {
        text = format!("[mcp error] {text}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_sanitizes_server_id() {
        assert_eq!(
            mcp_tool_name("my.server", "read_file"),
            "my_server_read_file"
        );
        assert_eq!(
            mcp_tool_name("filesystem", "read_file"),
            "filesystem_read_file"
        );
    }

    #[test]
    fn tool_name_is_capped() {
        let long = "x".repeat(100);
        let name = mcp_tool_name("server", &long);
        assert_eq!(name.len(), MAX_TOOL_NAME);
        assert!(name.starts_with("server_"));
    }

    #[test]
    fn tool_name_keeps_multibyte_tool_names_valid() {
        let name = mcp_tool_name("s", "工具");
        assert_eq!(name, "s_工具");
    }

    #[test]
    fn formats_error_results() {
        let mut result = CallToolResult::success(vec![ContentBlock::text("boom")]);
        result.is_error = Some(true);
        assert_eq!(format_result(&result), "[mcp error] boom");
    }
}

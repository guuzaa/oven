//! MCP (Model Context Protocol) support: config-side registration plus the
//! rmcp stdio client that bridges remote tools into the agent.

pub mod client;
mod registry;

pub use client::{DefaultMcpConnector, McpCaller, McpConnector, McpTool};
pub use registry::{McpError, McpRegistry, McpServerConfig};

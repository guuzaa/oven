//! MCP wiring tests: the server side is mocked with mockall, so no real MCP
//! process needs to be spawned.

use std::sync::Arc;

use async_trait::async_trait;
use mockall::mock;
use oven_agent::Tool;
use oven_app::AppBuilder;
use oven_app::config::AppConfig;
use oven_app::mcp::McpRegistry;
use oven_app::mcp::client::{McpCaller, McpConnector, McpTool};
use rmcp::model::ContentBlock as McpContentBlock;
use rmcp::model::{CallToolRequestParams, CallToolResult};

mock! {
    pub Caller {}
    #[async_trait]
    impl McpCaller for Caller {
        async fn call_tool(&self, params: CallToolRequestParams) -> Result<CallToolResult, String>;
    }
}

mock! {
    pub Connector {}
    #[async_trait]
    impl McpConnector for Connector {
        async fn connect(&self, registry: &McpRegistry, _root: &std::path::Path)
            -> Result<Vec<McpTool>, String>;
    }
}

#[tokio::test]
async fn mcp_tool_calls_through_mock_caller() {
    let mut caller = MockCaller::new();
    caller
        .expect_call_tool()
        .times(1)
        .withf(|params| {
            params.name == "echo"
                && params.arguments.as_ref().and_then(|a| a.get("text"))
                    == Some(&serde_json::json!("hi"))
        })
        .returning(|_| {
            Ok(CallToolResult::success(vec![McpContentBlock::text(
                "echo: hi",
            )]))
        });

    let tool = McpTool::new(
        "test",
        "echo",
        "Echo the given text back",
        serde_json::json!({"type": "object"}),
        Arc::new(caller),
    );
    assert_eq!(tool.name(), "test_echo");
    assert_eq!(tool.description(), "[mcp:test] Echo the given text back");

    let out = tool
        .run(&serde_json::json!({"text": "hi"}), None)
        .await
        .unwrap();
    assert_eq!(out, "echo: hi");
}

#[tokio::test]
async fn mcp_tool_caller_error_is_surfaced() {
    let mut caller = MockCaller::new();
    caller
        .expect_call_tool()
        .times(1)
        .returning(|_| Err("boom".to_string()));

    let tool = McpTool::new(
        "test",
        "echo",
        "Echo",
        serde_json::json!({"type": "object"}),
        Arc::new(caller),
    );
    let err = tool
        .run(&serde_json::json!({"text": "hi"}), None)
        .await
        .unwrap_err();
    assert_eq!(err.message, "mcp:test tool echo: boom");
}

#[tokio::test]
async fn mcp_connect_failure_propagates() {
    let mut connector = MockConnector::new();
    connector
        .expect_connect()
        .times(1)
        .returning(|_, _| Err("mcp 'bad': boom".to_string()));

    let cfg: AppConfig = toml::from_str(
        r#"
[provider]
name = "openai"

[providers.openai]
model = "gpt-4o-mini"
api_key = "sk-placeholder"
base_url = "http://127.0.0.1:1"

[mcps.bad]
command = "ignored"
"#,
    )
    .unwrap();
    let tmp = tempdir::TempDir::new("mcp-fail").unwrap();
    let app = AppBuilder::new(tmp.path())
        .with_config(cfg)
        .with_mcp_connector(Arc::new(connector));

    let err = match app.open().await {
        Ok(_) => panic!("spawn should fail when the mcp connector errors"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("mcp 'bad'"), "{err}");
}

#[tokio::test]
async fn mcp_http_invalid_header_rejected() {
    let cfg: AppConfig = toml::from_str(
        r#"
[provider]
name = "openai"

[providers.openai]
model = "gpt-4o-mini"
api_key = "sk-placeholder"
base_url = "http://127.0.0.1:1"

[mcps.test]
url = "http://127.0.0.1:1/mcp"

[mcps.test.headers]
"Bad Header" = "x"
"#,
    )
    .unwrap();
    let tmp = tempdir::TempDir::new("mcp-http-bad-header").unwrap();
    let app = AppBuilder::new(tmp.path()).with_config(cfg);
    let err = match app.open().await {
        Ok(_) => panic!("spawn should fail for an invalid http header"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("invalid header"), "{err}");
}

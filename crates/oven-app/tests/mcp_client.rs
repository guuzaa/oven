//! MCP wiring tests: the server side is mocked with mockall, so no real MCP
//! process needs to be spawned.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use mockall::mock;
use oven_agent::Tool;
use oven_app::config::AppConfig;
use oven_app::mcp::McpRegistry;
use oven_app::mcp::client::{McpCaller, McpConnector, McpTool};
use oven_app::{App, McpServerConfig};
use oven_llm::{
    ContentBlock, ModelId, ModelInfo, Provider, ProviderError, ProviderName, Request, Response,
    Role, StopReason, Usage,
};
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
async fn mcp_tools_mounted_on_agent() {
    let mut connector = MockConnector::new();
    connector.expect_connect().times(1).returning(|_, _| {
        let mut caller = MockCaller::new();
        caller.expect_call_tool().times(1).returning(|_| {
            Ok(CallToolResult::success(vec![McpContentBlock::text(
                "echo: hi",
            )]))
        });
        Ok(vec![McpTool::new(
            "test",
            "echo",
            "Echo the given text back",
            serde_json::json!({"type": "object"}),
            Arc::new(caller),
        )])
    });

    let tmp = tempdir::TempDir::new("mcp-agent").unwrap();
    let mut cfg = AppConfig::default();
    cfg.mcps.insert(
        "test".to_string(),
        McpServerConfig {
            command: "ignored".into(),
            args: vec![],
            env: Default::default(),
            ..Default::default()
        },
    );
    let app = App::new(tmp.path())
        .with_config(cfg)
        .with_mcp_connector(Arc::new(connector));

    let mock = MockProvider::new(vec![
        Response {
            id: "r1".into(),
            model: "mock".into(),
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "test_echo".into(),
                input: serde_json::json!({"text": "hi"}),
            }],
            stop_reason: Some(StopReason::ToolUse),
            usage: Some(usage(2, 1)),
        },
        text_response("done"),
    ]);

    let handle = app.spawn_with_provider(Box::new(mock)).await.unwrap();
    let out = handle.prompt("use the echo tool").await.unwrap();
    handle.shutdown().await;
    assert_eq!(out, "done");
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
model = "gpt-4o-mini"
api_key = "x"
base_url = "http://127.0.0.1:1"

[mcps.bad]
command = "ignored"
"#,
    )
    .unwrap();
    let tmp = tempdir::TempDir::new("mcp-fail").unwrap();
    let app = App::new(tmp.path())
        .with_config(cfg)
        .with_mcp_connector(Arc::new(connector));

    let err = match app.spawn().await {
        Ok(_) => panic!("spawn should fail when the mcp connector errors"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("mcp 'bad'"), "{err}");
}

#[tokio::test]
async fn mcp_http_server_mounted_on_agent() {
    use oven_agent::CancellationToken;
    use rmcp::model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock as McpContentBlock,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    };
    use rmcp::service::{RequestContext, RoleServer};
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    use rmcp::{ErrorData as McpError, ServerHandler};

    #[derive(Clone, Default)]
    struct HttpTestServer;

    impl ServerHandler for HttpTestServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, McpError> {
            Ok(ListToolsResult::with_all_items(vec![Tool::new(
                "echo",
                "Echo the given text back",
                rmcp::model::object(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "text to echo" }
                    },
                    "required": ["text"]
                })),
            )]))
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, McpError> {
            let text = request
                .arguments
                .as_ref()
                .and_then(|a| a.get("text"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Ok(CallToolResponse::Complete(CallToolResult::success(vec![
                McpContentBlock::text(format!("echo: {text}")),
            ])))
        }
    }

    let ct = CancellationToken::new();
    let service: StreamableHttpService<HttpTestServer, LocalSessionManager> =
        StreamableHttpService::new(|| Ok(HttpTestServer), Default::default(), {
            let mut cfg = StreamableHttpServerConfig::default();
            cfg.legacy_session_mode = true;
            cfg.cancellation_token = ct.child_token();
            cfg
        });
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn({
        let ct = ct.clone();
        async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move { ct.cancelled_owned().await })
                .await;
        }
    });

    let tmp = tempdir::TempDir::new("mcp-http").unwrap();
    let cfg: AppConfig = toml::from_str(&format!(
        r#"
[mcps.test]
url = "http://{addr}/mcp"
"#
    ))
    .unwrap();
    let app = App::new(tmp.path()).with_config(cfg);

    let mock = MockProvider::new(vec![
        Response {
            id: "r1".into(),
            model: "mock".into(),
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "test_echo".into(),
                input: serde_json::json!({"text": "hi"}),
            }],
            stop_reason: Some(StopReason::ToolUse),
            usage: Some(usage(2, 1)),
        },
        text_response("done"),
    ]);

    let handle = app.spawn_with_provider(Box::new(mock)).await.unwrap();
    let out = handle.prompt("use the echo tool").await.unwrap();
    handle.shutdown().await;
    assert_eq!(out, "done");

    ct.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn mcp_http_invalid_header_rejected() {
    let cfg: AppConfig = toml::from_str(
        r#"
[provider]
model = "gpt-4o-mini"
api_key = "x"
base_url = "http://127.0.0.1:1"

[mcps.test]
url = "http://127.0.0.1:1/mcp"

[mcps.test.headers]
"Bad Header" = "x"
"#,
    )
    .unwrap();
    let tmp = tempdir::TempDir::new("mcp-http-bad-header").unwrap();
    let app = App::new(tmp.path()).with_config(cfg);
    let err = match app.spawn().await {
        Ok(_) => panic!("spawn should fail for an invalid http header"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("invalid header"), "{err}");
}

fn usage(input: u32, output: u32) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: 0,
        reasoning_tokens: 0,
    }
}

fn text_response(text: &str) -> Response {
    Response {
        id: "resp".into(),
        model: "mock".into(),
        role: Role::Assistant,
        content: vec![ContentBlock::text(text)],
        stop_reason: Some(StopReason::EndTurn),
        usage: Some(usage(1, 1)),
    }
}

struct MockProvider {
    responses: Mutex<std::collections::VecDeque<Response>>,
}

impl MockProvider {
    fn new(responses: Vec<Response>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
        }
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn complete(&self, _req: &Request) -> Result<Response, ProviderError> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ProviderError::Api {
                status: 500,
                body: "no more mock responses".into(),
            })
    }

    async fn stream(
        &self,
        _req: &Request,
    ) -> Result<BoxStream<'static, Result<oven_llm::StreamEvent, ProviderError>>, ProviderError>
    {
        Err(ProviderError::Api {
            status: 500,
            body: "stream disabled in mock".into(),
        })
    }

    fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
        None
    }

    fn provider_name(&self) -> ProviderName {
        ProviderName::Custom("mock".into())
    }
}

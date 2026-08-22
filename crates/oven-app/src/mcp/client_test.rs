use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream::BoxStream;
use mockall::mock;
use oven_llm::{
    ContentBlock as LlmContentBlock, ModelId, ModelInfo, Provider, ProviderError, ProviderName,
    Request, Response, Role, StopReason, Usage,
};

use crate::App;
use crate::config::AppConfig;
use crate::mcp::{McpRegistry, client::*};
use crate::runtime::{AppId, spawn_runtime};

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
        content: vec![LlmContentBlock::text(text)],
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

async fn spawn_app(app: &App, provider: Box<dyn Provider>) -> crate::AppHandle {
    let agent = app.build_agent_with_provider(provider).await.unwrap();
    spawn_runtime(
        AppId::next(),
        agent,
        None,
        app.root().to_path_buf(),
        app.config().clone(),
        None,
    )
}

#[tokio::test]
async fn mcp_tools_mounted_on_agent() {
    let mut connector = MockConnector::new();
    connector.expect_connect().times(1).returning(|_, _| {
        let mut caller = MockCaller::new();
        caller.expect_call_tool().times(1).returning(|_| {
            Ok(CallToolResult::success(vec![ContentBlock::text(
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
        crate::McpServerConfig {
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
            content: vec![LlmContentBlock::ToolUse {
                id: "call-1".into(),
                name: "test_echo".into(),
                input: serde_json::json!({"text": "hi"}),
            }],
            stop_reason: Some(StopReason::ToolUse),
            usage: Some(usage(2, 1)),
        },
        text_response("done"),
    ]);

    let handle = spawn_app(&app, Box::new(mock)).await;
    let out = handle.prompt("use the echo tool").await.unwrap();
    handle.shutdown().await;
    assert_eq!(out, "done");
}

#[tokio::test]
async fn mcp_http_server_mounted_on_agent() {
    use oven_agent::CancellationToken;
    use rmcp::model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
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
                ContentBlock::text(format!("echo: {text}")),
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
            content: vec![LlmContentBlock::ToolUse {
                id: "call-1".into(),
                name: "test_echo".into(),
                input: serde_json::json!({"text": "hi"}),
            }],
            stop_reason: Some(StopReason::ToolUse),
            usage: Some(usage(2, 1)),
        },
        text_response("done"),
    ]);

    let handle = spawn_app(&app, Box::new(mock)).await;
    let out = handle.prompt("use the echo tool").await.unwrap();
    handle.shutdown().await;
    assert_eq!(out, "done");

    ct.cancel();
    server.await.unwrap();
}

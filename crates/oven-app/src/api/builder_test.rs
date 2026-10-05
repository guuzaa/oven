use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use oven_agent::{
    CancellationToken, ListModelsTool, MEMORY_PROMPT, NullSink, SpawnRequest, SubagentSpawner,
    TurnContext, TurnId,
};
use oven_llm::{
    ContentBlock, ModelId, ModelInfo, Provider, ProviderError, ProviderName, Request, Response,
    Role, StopReason, StreamEvent, Usage,
};
use oven_mem::{MemoryRoots, MemoryStore};

use crate::capabilities::memory::{MemoryForgetTool, MemoryReadTool, MemoryWriteTool};
use crate::core::config::AppConfig;

use super::AppBuilder;

const MEMORY_ID: &str = "proxy-requires-http2";
const MEMORY_DESCRIPTION: &str = "The internal proxy only speaks HTTP/2.";
const CATALOG_ENTRY: &str =
    "- workspace/proxy-requires-http2 The internal proxy only speaks HTTP/2.";
const MEMORY_TAG: &str = "<memory>";
const SUBAGENT_MARK: &str = "You are a subagent";
const EXPLORE_MARK: &str = "# Your role: explore";
const GENERAL_MARK: &str = "# Your role: general";

fn non_streaming(name: &ProviderName, id: &ModelId) -> Option<&'static ModelInfo> {
    static MODEL: std::sync::OnceLock<ModelInfo> = std::sync::OnceLock::new();
    match id.vendor() {
        Some(vendor) if !name.matches_vendor(vendor) => None,
        _ => Some(
            MODEL
                .get_or_init(|| ModelInfo::minimal("default", ProviderName::Custom("mock".into()))),
        ),
    }
}

fn memory_file() -> String {
    format!("---\nkind: fact\ndescription: {MEMORY_DESCRIPTION}\n---\n\nbody\n")
}

fn write_workspace_memory(root: &Path) {
    let dir = root.join(".oven").join("memory");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{MEMORY_ID}.md")), memory_file()).unwrap();
}

struct CapturedPrompt {
    driver: String,
    explore: String,
    general: String,
    driver_tools: Vec<String>,
    explore_tools: Vec<String>,
    general_tools: Vec<String>,
}

#[derive(Clone)]
struct SeenCall {
    system: String,
    tools: Vec<String>,
}

struct CaptureProvider {
    calls: Arc<Mutex<Vec<SeenCall>>>,
}

#[async_trait]
impl Provider for CaptureProvider {
    async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
        self.calls.lock().unwrap().push(SeenCall {
            system: req.system.clone().unwrap_or_default(),
            tools: req.tools.iter().map(|tool| tool.name.clone()).collect(),
        });
        Ok(Response {
            id: "resp".into(),
            model: "mock".into(),
            role: Role::Assistant,
            content: vec![ContentBlock::text("ok")],
            stop_reason: Some(StopReason::EndTurn),
            usage: Some(Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
            }),
        })
    }

    async fn stream(
        &self,
        _req: &Request,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        Err(ProviderError::Api {
            status: 500,
            body: "stream disabled in mock".into(),
        })
    }

    fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
        non_streaming(&self.provider_name(), id)
    }

    fn provider_name(&self) -> ProviderName {
        ProviderName::Custom("mock".into())
    }
}

/// Memory roots the caller owns, so the developer's `~/.oven/memory` never
/// leaks into the prompt these tests assert on.
async fn isolated_app(root: &Path, config: AppConfig) -> AppBuilder {
    let mut app = AppBuilder::new(root).with_config(config).await;
    let store = MemoryStore::load(MemoryRoots {
        workspace: root.join(".oven").join("memory"),
        user: Some(root.join("user-memory")),
    })
    .await;
    app.set_memory(Arc::new(store));
    app
}

async fn capture(root: &Path, config: AppConfig) -> CapturedPrompt {
    capture_app(isolated_app(root, config).await).await
}

async fn capture_app(app: AppBuilder) -> CapturedPrompt {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut agents = app
        .build_agent_with_provider(Box::new(CaptureProvider {
            calls: Arc::clone(&calls),
        }))
        .await
        .unwrap();
    let ctx = TurnContext::new(
        TurnId::next(),
        CancellationToken::new(),
        agents.main.selection(),
    );
    agents
        .main
        .step(&mut NullSink, &ctx)
        .await
        .expect("driver step");
    for (role, prompt) in [("explore", "look"), ("general", "do")] {
        agents
            .subagents
            .spawn(SpawnRequest {
                role: role.into(),
                label: prompt.into(),
                prompt: prompt.into(),
                background: true,
                model: agents.main.model(),
                reasoning_effort: None,
                parent_turn: TurnId::next(),
                cancellation: CancellationToken::new(),
            })
            .await
            .unwrap()
            .join()
            .await
            .unwrap();
    }
    let calls = calls.lock().unwrap().clone();
    let call = |mark: &str| {
        calls
            .iter()
            .find(|call| call.system.contains(mark))
            .expect("matching request")
    };
    let driver = calls
        .iter()
        .find(|call| !call.system.contains(SUBAGENT_MARK))
        .expect("driver system");
    let explore = call(EXPLORE_MARK);
    let general = call(GENERAL_MARK);
    CapturedPrompt {
        driver: driver.system.clone(),
        explore: explore.system.clone(),
        general: general.system.clone(),
        driver_tools: driver.tools.clone(),
        explore_tools: explore.tools.clone(),
        general_tools: general.tools.clone(),
    }
}

fn base_prompt(app: &AppBuilder) -> String {
    app.system_without_memory()
}

#[tokio::test]
async fn catalog_reaches_driver_and_role_prompts() {
    let tmp = tempdir::TempDir::new("memory-prompt").unwrap();
    write_workspace_memory(tmp.path());
    let captured = capture(tmp.path(), AppConfig::default()).await;
    assert!(
        captured.driver.contains(&format!("\n\n{MEMORY_TAG}\n")),
        "catalog is separated by a blank line"
    );
    assert!(captured.driver.contains(CATALOG_ENTRY));
    assert_eq!(
        captured.explore,
        super::role_system(
            &captured.driver,
            super::EXPLORE_ROLE,
            super::EXPLORE_GUIDANCE
        )
    );
    assert_eq!(
        captured.general,
        super::role_system(
            &captured.driver,
            super::GENERAL_ROLE,
            super::GENERAL_GUIDANCE
        )
    );
    assert!(captured.explore.contains(CATALOG_ENTRY));
    assert!(captured.general.contains(CATALOG_ENTRY));
}

#[tokio::test]
async fn guidance_precedes_catalog() {
    let tmp = tempdir::TempDir::new("memory-guidance-order").unwrap();
    write_workspace_memory(tmp.path());
    let captured = capture(tmp.path(), AppConfig::default()).await;
    let guidance = captured
        .driver
        .find(MEMORY_PROMPT)
        .expect("memory guidance");
    let catalog = captured.driver.find(MEMORY_TAG).expect("memory catalog");
    assert!(guidance < catalog);
}

#[tokio::test]
async fn empty_store_adds_guidance_without_catalog() {
    let tmp = tempdir::TempDir::new("memory-prompt-empty").unwrap();
    let app = isolated_app(tmp.path(), AppConfig::default()).await;
    let expected = super::append_block(base_prompt(&app), MEMORY_PROMPT);
    let empty = capture(tmp.path(), AppConfig::default()).await;
    assert_eq!(empty.driver, expected);
    assert!(!empty.driver.contains(MEMORY_TAG));
    assert_eq!(
        empty.explore,
        super::role_system(&expected, super::EXPLORE_ROLE, super::EXPLORE_GUIDANCE)
    );
    assert_eq!(
        empty.general,
        super::role_system(&expected, super::GENERAL_ROLE, super::GENERAL_GUIDANCE)
    );
}

#[tokio::test]
async fn unloaded_matches_prompt_without_memory() {
    let tmp = tempdir::TempDir::new("memory-prompt-off").unwrap();
    write_workspace_memory(tmp.path());
    let app = AppBuilder::new(tmp.path())
        .with_config(AppConfig::default())
        .await;
    let expected = base_prompt(&app);
    let disabled = capture_app(app).await;
    assert_eq!(disabled.driver, expected);
    assert!(!disabled.driver.contains(MEMORY_PROMPT));
    assert!(!disabled.driver.contains(MEMORY_TAG));
    assert_eq!(
        disabled.explore,
        super::role_system(&expected, super::EXPLORE_ROLE, super::EXPLORE_GUIDANCE)
    );
    assert_eq!(
        disabled.general,
        super::role_system(&expected, super::GENERAL_ROLE, super::GENERAL_GUIDANCE)
    );
    assert!(
        !disabled
            .explore_tools
            .iter()
            .any(|name| name == MemoryReadTool::NAME)
    );
}

#[tokio::test]
async fn memory_read_is_in_the_explore_tool_set() {
    let tmp = tempdir::TempDir::new("memory-read-explore").unwrap();
    let captured = capture(tmp.path(), AppConfig::default()).await;
    assert!(
        captured
            .explore_tools
            .iter()
            .any(|name| name == MemoryReadTool::NAME)
    );
}

fn has_tool(tools: &[String], name: &str) -> bool {
    tools.iter().any(|tool| tool == name)
}

#[tokio::test]
async fn memory_writers_are_excluded_from_subagent_roles() {
    let tmp = tempdir::TempDir::new("memory-write-roles").unwrap();
    let captured = capture(tmp.path(), AppConfig::default()).await;
    assert!(has_tool(&captured.driver_tools, MemoryWriteTool::NAME));
    assert!(!has_tool(&captured.explore_tools, MemoryWriteTool::NAME));
    assert!(!has_tool(&captured.general_tools, MemoryWriteTool::NAME));
    assert!(has_tool(&captured.driver_tools, MemoryForgetTool::NAME));
    assert!(!has_tool(&captured.explore_tools, MemoryForgetTool::NAME));
    assert!(!has_tool(&captured.general_tools, MemoryForgetTool::NAME));
}

#[tokio::test]
async fn list_models_is_mounted_for_the_driver_only() {
    let tmp = tempdir::TempDir::new("model-list-mount").unwrap();
    let captured = capture(tmp.path(), AppConfig::default()).await;
    assert!(has_tool(&captured.driver_tools, ListModelsTool::NAME));
    assert!(!has_tool(&captured.explore_tools, ListModelsTool::NAME));
    assert!(!has_tool(&captured.general_tools, ListModelsTool::NAME));

    let off = capture(
        tmp.path(),
        AppConfig {
            subagents: crate::core::config::SubagentConfig {
                enabled: false,
                ..AppConfig::default().subagents
            },
            ..AppConfig::default()
        },
    )
    .await;
    assert!(!has_tool(&off.driver_tools, ListModelsTool::NAME));
}

struct ScriptedProvider {
    systems: Arc<Mutex<Vec<String>>>,
    responses: Mutex<VecDeque<Response>>,
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
        self.systems
            .lock()
            .unwrap()
            .push(req.system.clone().unwrap_or_default());
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
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        Err(ProviderError::Api {
            status: 500,
            body: "stream disabled in mock".into(),
        })
    }

    fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
        non_streaming(&self.provider_name(), id)
    }

    fn provider_name(&self) -> ProviderName {
        ProviderName::Custom("mock".into())
    }
}

fn tool_response(name: &str, input: serde_json::Value) -> Response {
    Response {
        id: "resp".into(),
        model: "mock".into(),
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "c1".into(),
            name: name.into(),
            input,
            raw_arguments: None,
        }],
        stop_reason: Some(StopReason::ToolUse),
        usage: Some(Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
        }),
    }
}

fn text_response(text: &str) -> Response {
    Response {
        id: "resp".into(),
        model: "mock".into(),
        role: Role::Assistant,
        content: vec![ContentBlock::text(text)],
        stop_reason: Some(StopReason::EndTurn),
        usage: Some(Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
        }),
    }
}

#[tokio::test]
async fn write_does_not_change_the_driver_system_prompt() {
    let tmp = tempdir::TempDir::new("memory-write-frozen").unwrap();
    write_workspace_memory(tmp.path());
    let systems = Arc::new(Mutex::new(Vec::new()));
    let app = isolated_app(tmp.path(), AppConfig::default()).await;
    let mut agents = app
        .build_agent_with_provider(Box::new(ScriptedProvider {
            systems: Arc::clone(&systems),
            responses: Mutex::new(VecDeque::from([
                tool_response(
                    MemoryWriteTool::NAME,
                    serde_json::json!({
                        "scope": "workspace",
                        "id": MEMORY_ID,
                        "kind": "fact",
                        "description": "brand new description",
                        "body": "updated body\n",
                    }),
                ),
                text_response("done"),
            ])),
        }))
        .await
        .unwrap();
    let ctx = TurnContext::new(
        TurnId::next(),
        CancellationToken::new(),
        agents.main.selection(),
    );
    agents.main.step(&mut NullSink, &ctx).await.unwrap();
    agents.main.step(&mut NullSink, &ctx).await.unwrap();
    let systems = systems.lock().unwrap().clone();
    assert_eq!(systems.len(), 2);
    assert_eq!(systems[0], systems[1]);
    assert!(systems[0].contains(CATALOG_ENTRY));
    assert!(!systems[0].contains("brand new description"));
    let raw = std::fs::read_to_string(
        tmp.path()
            .join(".oven")
            .join("memory")
            .join(format!("{MEMORY_ID}.md")),
    )
    .unwrap();
    assert!(raw.contains("brand new description"));
}

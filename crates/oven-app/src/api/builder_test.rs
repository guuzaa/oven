use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use oven_agent::{CancellationToken, NullSink, SpawnRequest, SubagentSpawner, TurnContext, TurnId};
use oven_llm::{
    ModelId, ModelInfo, Provider, ProviderError, ProviderName, Request, Response, Role, StopReason,
    StreamEvent, Usage,
};

use crate::core::config::{AppConfig, MemoryConfig};

use super::AppBuilder;

const MEMORY_ID: &str = "proxy-requires-http2";
const MEMORY_DESCRIPTION: &str = "The internal proxy only speaks HTTP/2.";
const CATALOG_ENTRY: &str =
    "- workspace/proxy-requires-http2 The internal proxy only speaks HTTP/2.";
const MEMORY_TAG: &str = "<memory>";
const SUBAGENT_MARK: &str = "You are a subagent";
const EXPLORE_MARK: &str = "# Your role: explore";
const GENERAL_MARK: &str = "# Your role: general";

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
}

struct CaptureProvider {
    systems: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for CaptureProvider {
    async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
        self.systems
            .lock()
            .unwrap()
            .push(req.system.clone().unwrap_or_default());
        Ok(Response {
            id: "resp".into(),
            model: "mock".into(),
            role: Role::Assistant,
            content: vec![oven_llm::ContentBlock::text("ok")],
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

    fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
        None
    }

    fn provider_name(&self) -> ProviderName {
        ProviderName::Custom("mock".into())
    }
}

async fn capture(root: &Path, config: AppConfig) -> CapturedPrompt {
    let systems = Arc::new(Mutex::new(Vec::new()));
    let app = AppBuilder::new(root).with_config(config).await;
    let mut agents = app
        .build_agent_with_provider(Box::new(CaptureProvider {
            systems: Arc::clone(&systems),
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
    let systems = systems.lock().unwrap().clone();
    CapturedPrompt {
        driver: systems
            .iter()
            .find(|system| !system.contains(SUBAGENT_MARK))
            .cloned()
            .expect("driver system"),
        explore: systems
            .iter()
            .find(|system| system.contains(EXPLORE_MARK))
            .cloned()
            .expect("explore system"),
        general: systems
            .iter()
            .find(|system| system.contains(GENERAL_MARK))
            .cloned()
            .expect("general system"),
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
async fn disabled_or_empty_matches_prompt_without_memory() {
    let tmp = tempdir::TempDir::new("memory-prompt-off").unwrap();
    let app = AppBuilder::new(tmp.path())
        .with_config(AppConfig::default())
        .await;
    let expected = base_prompt(&app);
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

    write_workspace_memory(tmp.path());
    let disabled = capture(
        tmp.path(),
        AppConfig {
            memory: MemoryConfig { enabled: false },
            ..AppConfig::default()
        },
    )
    .await;
    assert_eq!(disabled.driver, expected);
    assert_eq!(disabled.explore, empty.explore);
    assert_eq!(disabled.general, empty.general);
}

use crate::App;
use crate::runtime::*;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream::BoxStream;
use oven_llm::{
    ContentBlock, ModelId, ModelInfo, Provider, ProviderError, ProviderName, Request, Response,
    Role, Router, StopReason, StreamEvent, Usage,
};
use std::path::PathBuf;
use std::sync::Arc;

fn agent_from(provider: Box<dyn Provider>) -> Agent {
    let mut router = Router::new();
    router.register(provider);
    Agent::new(router, Vec::new())
}

async fn spawn_app(app: &App, provider: Box<dyn Provider>) -> AppHandle {
    let agent = app.build_agent_with_provider(provider).await.unwrap();
    spawn_runtime(
        AppId::next(),
        agent,
        None,
        app.config().provider.effective_model(),
        app.root().to_path_buf(),
        app.config().clone(),
        None,
    )
}

async fn spawn_app_session(app: &App, provider: Box<dyn Provider>, session: Session) -> AppHandle {
    let prior = session.load_records().unwrap();
    let mut agent = app.build_agent_with_provider(provider).await.unwrap();
    let records: Vec<_> = prior
        .iter()
        .filter(|r| !matches!(r, Record::Message { message, .. } if message.role == Role::System))
        .cloned()
        .collect();
    agent.restore_history(records);
    hydrate_session(&mut agent, &prior);
    agent.ensure_session_meta(canonical_root(app.root()));
    spawn_runtime(
        AppId::next(),
        agent,
        Some(session),
        app.config().provider.effective_model(),
        app.root().to_path_buf(),
        app.config().clone(),
        None,
    )
}

fn text_response(text: &str) -> Response {
    Response {
        id: "resp".into(),
        model: "mock".into(),
        role: Role::Assistant,
        content: vec![ContentBlock::text(text)],
        stop_reason: Some(StopReason::EndTurn),
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
        }),
    }
}

struct MockProvider {
    responses: std::sync::Mutex<std::collections::VecDeque<Response>>,
}

impl MockProvider {
    fn new(responses: Vec<Response>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into()),
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

/// Provider that records every request model and echoes it back, so tests
/// can observe which provider/model handled a turn.
struct RecordingProvider {
    seen: Arc<Mutex<Vec<String>>>,
}

impl RecordingProvider {
    fn new(seen: Arc<Mutex<Vec<String>>>) -> Self {
        Self { seen }
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("request:{}", req.model.as_str()));
        Ok(text_response(&format!("echo:{}", req.model.as_str())))
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

fn recorder() -> Arc<Mutex<Vec<String>>> {
    Arc::new(Mutex::new(Vec::new()))
}

#[tokio::test]
async fn spawn_prompt_emits_done_and_idle() {
    let tmp = tempdir::TempDir::new("app-runtime").unwrap();
    let app = App::new(tmp.path());
    let mock = MockProvider::new(vec![text_response("hello")]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let mut rx = handle.subscribe();
    assert!(
        handle.session_id().is_none(),
        "no session id without persistence"
    );
    let text = handle.prompt("hi").await.unwrap();
    assert_eq!(text, "hello");

    let mut saw_done = false;
    let mut saw_idle = false;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            AppEvent::Agent {
                event: AgentEvent::Done { text, .. },
                ..
            } if text == "hello" => saw_done = true,
            AppEvent::Idle { .. } => saw_idle = true,
            _ => {}
        }
    }
    assert!(saw_done);
    assert!(saw_idle);

    handle.shutdown().await;
}

#[tokio::test]
async fn handle_exposes_slash_commands() {
    let tmp = tempdir::TempDir::new("app-runtime-slash").unwrap();
    let app = App::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let names: Vec<&str> = handle
        .slash_commands()
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(names, ["clear", "exit", "model", "setup", "plan"]);
    assert!(handle.slash_commands().iter().all(|(_, d)| !d.is_empty()));

    handle.shutdown().await;
}

#[tokio::test]
async fn plan_slash_on_idle_switches() {
    let tmp = tempdir::TempDir::new("app-runtime-plan").unwrap();
    let app = App::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let mut rx = handle.subscribe();
    let status = handle.prompt("/plan").await.unwrap();
    assert!(status.contains("current mode: agent"));
    assert!(status.contains("0 todos"));

    let _ = handle.prompt("/plan on").await.unwrap();
    let mut saw_plan = false;
    while let Ok(ev) = rx.try_recv() {
        if matches!(
            ev,
            AppEvent::ModeChanged {
                mode: AgentMode::Plan,
                ..
            }
        ) {
            saw_plan = true;
        }
    }
    assert!(saw_plan, "idle /plan on must emit ModeChanged(Plan)");

    let status = handle.prompt("/plan").await.unwrap();
    assert!(status.contains("current mode: plan"));

    handle.shutdown().await;
}

#[tokio::test]
async fn model_slash_switch_uses_request_model() {
    let seen = recorder();
    let agent = agent_from(Box::new(RecordingProvider::new(seen.clone()))).with_model("gpt-4o");
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        None,
        "gpt-4o".into(),
        PathBuf::from("/tmp"),
        AppConfig::default(),
        None,
    );

    let mut rx = handle.subscribe();
    let out = handle.prompt("/model gpt-4o-turbo low").await.unwrap();
    assert_eq!(out, "model switched to mock/gpt-4o-turbo (effort: low)");
    let mut saw_reply = false;
    let mut saw_done = false;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            AppEvent::Notify { text, .. }
                if text == "model switched to mock/gpt-4o-turbo (effort: low)" =>
            {
                saw_reply = true;
            }
            AppEvent::Agent {
                event: AgentEvent::Done { .. },
                ..
            } => saw_done = true,
            _ => {}
        }
    }
    assert!(saw_reply);
    assert!(!saw_done);
    // The switch only changes the model carried by subsequent requests;
    // the provider object is never rebuilt or replaced.
    assert_eq!(
        handle.prompt("hello").await.unwrap(),
        "echo:mock/gpt-4o-turbo"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn slash_reply_emits_reply_not_done() {
    let tmp = tempdir::TempDir::new("app-runtime-slash-reply").unwrap();
    let app = App::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let mut rx = handle.subscribe();
    let out = handle.prompt("/model").await.unwrap();
    assert!(out.contains("current model"));

    let mut saw_reply = false;
    let mut saw_done = false;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            AppEvent::Notify { text, .. } if text.contains("current model") => {
                saw_reply = true;
            }
            AppEvent::Agent {
                event: AgentEvent::Done { .. },
                ..
            } => saw_done = true,
            _ => {}
        }
    }
    assert!(saw_reply);
    assert!(!saw_done);
    handle.shutdown().await;
}

#[tokio::test]
async fn slash_exit_emits_exit_event() {
    let tmp = tempdir::TempDir::new("app-runtime-exit").unwrap();
    let app = App::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let mut rx = handle.subscribe();
    handle.send(AppCmd::UserInput("/exit".into())).unwrap();

    let mut saw_exit = false;
    while let Some(ev) = rx.recv().await {
        if let AppEvent::Exit { .. } = ev {
            saw_exit = true;
        }
        if let AppEvent::Idle { .. } = ev {
            break;
        }
    }
    assert!(saw_exit);
    handle.shutdown().await;
}

#[tokio::test]
async fn slash_clear_does_not_call_provider() {
    let tmp = tempdir::TempDir::new("app-runtime-clear-noprovider").unwrap();
    let app = App::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;
    assert_eq!(handle.prompt("/clear").await.unwrap(), "history cleared");
    handle.shutdown().await;
}

#[tokio::test]
async fn slash_clear_emits_history_cleared_and_resets_usage() {
    let tmp = tempdir::TempDir::new("app-runtime-clear-events").unwrap();
    let app = App::new(tmp.path());
    let mock = MockProvider::new(vec![text_response("one")]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    assert_eq!(handle.prompt("hello").await.unwrap(), "one");

    let mut rx = handle.subscribe();
    let out = handle.prompt("/clear").await.unwrap();
    assert_eq!(out, "history cleared");

    let mut saw_cleared = false;
    let mut saw_todos_cleared = false;
    let mut done_usage = None;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            AppEvent::Agent {
                event: AgentEvent::HistoryCleared { .. },
                ..
            } => saw_cleared = true,
            AppEvent::Agent {
                event: AgentEvent::TodoUpdated { items, .. },
                ..
            } if items.is_empty() => saw_todos_cleared = true,
            AppEvent::Agent {
                event: AgentEvent::Done { usage, text, .. },
                ..
            } if text == "history cleared" => done_usage = Some(usage),
            _ => {}
        }
    }
    assert!(saw_cleared);
    assert!(saw_todos_cleared);
    let usage = done_usage.expect("done after /clear");
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.output_tokens, 0);
    handle.shutdown().await;
}

#[tokio::test]
async fn slash_exit_returns_goodbye() {
    let tmp = tempdir::TempDir::new("app-runtime-exit-prompt").unwrap();
    let app = App::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;
    assert_eq!(handle.prompt("/exit").await.unwrap(), "goodbye");
    handle.shutdown().await;
}

#[tokio::test]
async fn model_slash_model_only_keeps_effort() {
    let seen = recorder();
    let agent = agent_from(Box::new(RecordingProvider::new(seen.clone())))
        .with_model("gpt-4o")
        .with_reasoning_effort(oven_llm::ReasoningEffort::Low);
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        None,
        "gpt-4o".into(),
        PathBuf::from("/tmp"),
        AppConfig::default(),
        None,
    );

    let out = handle.prompt("/model gpt-4o-turbo").await.unwrap();
    assert_eq!(out, "model switched to mock/gpt-4o-turbo (effort: low)");
    assert_eq!(
        handle.prompt("hello").await.unwrap(),
        "echo:mock/gpt-4o-turbo"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn setup_slash_persists_and_registers_provider() {
    let tmp = tempdir::TempDir::new("app-runtime-setup").unwrap();
    let cfg_path = tmp.path().join("config.toml");
    let agent = agent_from(Box::new(MockProvider::new(vec![])));
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        None,
        "default".into(),
        tmp.path().to_path_buf(),
        AppConfig::default(),
        Some(cfg_path.clone()),
    );

    let mut rx = handle.subscribe();
    handle
        .send(AppCmd::UserInput(
            "/setup name=deepseek api_key=sk-test".into(),
        ))
        .unwrap();
    let mut out = String::new();
    loop {
        match rx.recv().await {
            Some(AppEvent::Agent {
                event: AgentEvent::Done { text, .. },
                ..
            }) => out = text,
            Some(AppEvent::Notify { text, .. }) => out = text,
            Some(AppEvent::Idle { .. }) => break,
            Some(_) => {}
            None => panic!("channel closed before idle"),
        }
    }
    assert!(out.contains("provider updated"));
    assert!(out.contains("name=deepseek"));
    assert!(!out.contains("kind="));
    assert!(out.contains("model=deepseek-v4-flash"));
    assert!(out.contains("base_url=https://api.deepseek.com"));
    assert!(out.contains("api_key=(set)"));
    assert!(!out.contains("sk-test"));
    assert!(out.contains(cfg_path.to_str().unwrap()));

    let saved = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(saved.contains("name = \"deepseek\""));
    assert!(!saved.contains("kind"));
    assert!(saved.contains("model = \"deepseek-v4-flash\""));
    assert!(saved.contains("base_url = \"https://api.deepseek.com\""));
    assert!(saved.contains("api_key = \"sk-test\""));
    assert!(saved.contains("reasoning_effort = \"medium\""));
    assert!(out.contains("reasoning_effort=medium"));

    let current = handle.prompt("/model").await.unwrap();
    assert!(current.contains("reasoning effort: medium"));
    handle.shutdown().await;
}

#[tokio::test]
async fn setup_registers_without_dropping_existing_vendor() {
    let seen = recorder();
    let tmp = tempdir::TempDir::new("app-runtime-setup-keep").unwrap();
    let agent = agent_from(Box::new(RecordingProvider::new(seen.clone()))).with_model("mock/echo");
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        None,
        "mock/echo".into(),
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );

    let mut rx = handle.subscribe();
    handle
        .send(AppCmd::UserInput(
            "/setup name=deepseek api_key=sk-test".into(),
        ))
        .unwrap();
    loop {
        match rx.recv().await {
            Some(AppEvent::Idle { .. }) => break,
            Some(_) => {}
            None => panic!("channel closed before idle"),
        }
    }
    let switched = handle.prompt("/model mock/echo").await.unwrap();
    assert!(
        switched.contains("model switched to mock/echo"),
        "{switched}"
    );
    assert_eq!(handle.prompt("hello").await.unwrap(), "echo:mock/echo");
    handle.shutdown().await;
}

#[tokio::test]
async fn model_slash_persists_model_and_effort() {
    let tmp = tempdir::TempDir::new("app-runtime-model-save").unwrap();
    let cfg_path = tmp.path().join("config.toml");
    let agent = agent_from(Box::new(MockProvider::new(vec![])));
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        None,
        "default".into(),
        tmp.path().to_path_buf(),
        AppConfig::default(),
        Some(cfg_path.clone()),
    );

    let out = handle.prompt("/model gpt-4o-turbo high").await.unwrap();
    assert!(out.contains("model switched to mock/gpt-4o-turbo (effort: high)"));
    assert!(out.contains(cfg_path.to_str().unwrap()));

    let saved = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(saved.contains("model = \"gpt-4o-turbo\""));
    assert!(saved.contains("reasoning_effort = \"high\""));
    handle.shutdown().await;
}

#[tokio::test]
async fn setup_keeps_existing_reasoning_effort() {
    let tmp = tempdir::TempDir::new("app-runtime-setup-keep-effort").unwrap();
    let cfg_path = tmp.path().join("config.toml");
    let agent = agent_from(Box::new(MockProvider::new(vec![])))
        .with_reasoning_effort(oven_llm::ReasoningEffort::High);
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        None,
        "default".into(),
        tmp.path().to_path_buf(),
        AppConfig {
            provider: ProviderConfig {
                reasoning_effort: Some(oven_llm::ReasoningEffort::High),
                ..Default::default()
            },
            ..AppConfig::default()
        },
        Some(cfg_path.clone()),
    );

    let mut rx = handle.subscribe();
    handle
        .send(AppCmd::UserInput(
            "/setup name=deepseek api_key=sk-test".into(),
        ))
        .unwrap();
    loop {
        match rx.recv().await {
            Some(AppEvent::Idle { .. }) => break,
            Some(_) => {}
            None => panic!("channel closed before idle"),
        }
    }
    let current = handle.prompt("/model").await.unwrap();
    assert!(current.contains("reasoning effort: high"));
    let saved = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(!saved.contains("reasoning_effort"));
    handle.shutdown().await;
}

#[tokio::test]
async fn spawn_session_without_api_key_starts() {
    let tmp = tempdir::TempDir::new("app-first-run").unwrap();
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let app = App::new(tmp.path());
    let handle = app.spawn_session_in(&dir, None).await.unwrap();
    handle.shutdown().await;
}

#[tokio::test]
async fn spawn_without_api_key_still_errors() {
    let tmp = tempdir::TempDir::new("app-headless-no-key").unwrap();
    let app = App::new(tmp.path());
    if !app.config().provider.needs_setup() {
        return;
    }
    let err = match app.spawn().await {
        Ok(_) => panic!("headless spawn should fail without an API key"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("no API key"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn setup_slash_rejects_kind() {
    let tmp = tempdir::TempDir::new("app-runtime-setup-bad").unwrap();
    let app = App::new(tmp.path());
    let handle = spawn_app(&app, Box::new(MockProvider::new(vec![]))).await;
    let err = handle.prompt("/setup kind=chat").await.unwrap_err();
    assert!(err.to_string().contains("kind is no longer used"));
    handle.shutdown().await;
}

#[tokio::test]
async fn spawn_applies_configured_reasoning_effort() {
    let tmp = tempdir::TempDir::new("app-spawn-effort").unwrap();
    let app = App::new(tmp.path()).with_config(AppConfig {
        provider: ProviderConfig {
            reasoning_effort: Some(oven_llm::ReasoningEffort::Medium),
            ..Default::default()
        },
        ..AppConfig::default()
    });
    let handle = spawn_app(&app, Box::new(MockProvider::new(vec![]))).await;
    let out = handle.prompt("/model").await.unwrap();
    assert!(out.contains("reasoning effort: medium"));
    handle.shutdown().await;
}

#[tokio::test]
async fn session_persists_across_spawns() {
    let tmp = tempdir::TempDir::new("app-runtime-sess").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock1 = MockProvider::new(vec![text_response("one")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock1), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    handle.shutdown().await;

    let loaded = Session::open(&dir, "s1").unwrap().load().unwrap();
    assert!(loaded.iter().any(|m| {
        m.role == Role::User
            && m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text == "first"))
    }));
    assert!(loaded.iter().any(|m| {
        m.role == Role::Assistant
            && m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text == "one"))
    }));

    let mock2 = MockProvider::new(vec![text_response("two")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock2), session).await;
    assert_eq!(handle.prompt("second").await.unwrap(), "two");
    handle.shutdown().await;

    let loaded = Session::open(&dir, "s1").unwrap().load().unwrap();
    assert_eq!(loaded.iter().filter(|m| m.role == Role::User).count(), 2);
}

#[tokio::test]
async fn resumed_session_restores_usage_and_rewind_rolls_it_back() {
    let tmp = tempdir::TempDir::new("app-runtime-resume-usage").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    // First process: two turns, each mocked as 10 in / 5 out.
    let mock1 = MockProvider::new(vec![text_response("one"), text_response("two")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock1), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert_eq!(handle.prompt("second").await.unwrap(), "two");
    handle.shutdown().await;

    // The persisted file carries one TokenUsage record per turn; the
    // cumulative sum survives the restart.
    let records = Session::open(&dir, "s1").unwrap().load_records().unwrap();
    let persisted: Usage = records
        .iter()
        .filter_map(|r| match r {
            Record::TokenUsage { usage, .. } => Some(*usage),
            _ => None,
        })
        .fold(Usage::default(), |acc, u| acc + u);
    assert_eq!((persisted.input_tokens, persisted.output_tokens), (20, 10));

    // Second process: resume, then rewind the last exchange.
    let mock2 = MockProvider::new(vec![text_response("three")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock2), session).await;
    // The restored cumulative usage is visible on the handle immediately,
    // before any new turn completes (the TUI seeds its status bar from
    // this snapshot).
    assert_eq!(
        (
            handle.total_usage().input_tokens,
            handle.total_usage().output_tokens
        ),
        (20, 10)
    );
    let mut sub = handle.subscribe();
    handle.send(AppCmd::Rewind).unwrap();
    let (text, messages, usage) = wait_rewound(&mut sub).await;
    assert_eq!(text.as_deref(), Some("second"));
    assert_eq!(user_texts(&messages), vec!["first"]);
    assert_eq!((usage.input_tokens, usage.output_tokens), (10, 5));

    // A fresh turn keeps counting from the restored cumulative total.
    assert_eq!(handle.prompt("third").await.unwrap(), "three");
    handle.send(AppCmd::Rewind).unwrap();
    let (text, _, usage) = wait_rewound(&mut sub).await;
    assert_eq!(text.as_deref(), Some("third"));
    assert_eq!((usage.input_tokens, usage.output_tokens), (10, 5));

    handle.shutdown().await;
}

#[tokio::test]
async fn slash_clear_starts_new_session() {
    let tmp = tempdir::TempDir::new("app-runtime-clear").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    // Turn 1 persists a message so the file is non-empty.
    let mock = MockProvider::new(vec![text_response("one"), text_response("fresh")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");

    // `/clear` switches the runtime to a fresh uuid v7 session.
    let mut rx = handle.subscribe();
    handle.send(AppCmd::UserInput("/clear".into())).unwrap();
    while let Some(ev) = rx.recv().await {
        if let AppEvent::Idle { .. } = ev {
            break;
        }
    }

    // Turn 3 continues in the same handle and persists to the new session.
    assert_eq!(handle.prompt("hello").await.unwrap(), "fresh");
    let sid_after_clear = handle.session_id().expect("session id present");
    assert!(uuid::Uuid::parse_str(&sid_after_clear).is_ok());
    handle.shutdown().await;

    let old = Session::open(&dir, "s1").unwrap().load().unwrap();
    assert!(old.iter().any(|m| {
        m.role == Role::User
            && m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text == "first"))
    }));
    assert!(!old.iter().any(|m| {
        m.role == Role::User
            && m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text == "hello"))
    }));

    let mut fresh_ids: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().to_string();
        if let Some(stem) = name.strip_suffix(".jsonl")
            && stem != "s1"
        {
            let parsed = uuid::Uuid::parse_str(stem).expect("session id must be a uuid");
            assert_eq!(parsed.get_version_num(), 7);
            fresh_ids.push(stem.to_string());
        }
    }
    assert_eq!(fresh_ids.len(), 1, "expected one fresh uuid session file");
    assert_eq!(fresh_ids, [sid_after_clear]);
    let fresh = Session::open(&dir, &fresh_ids[0]).unwrap().load().unwrap();
    assert_eq!(fresh.iter().filter(|m| m.role == Role::User).count(), 1);
    assert!(fresh.iter().any(|m| {
        m.role == Role::User
            && m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text == "hello"))
    }));
}

#[tokio::test]
async fn spawn_session_creates_uuid_when_id_missing() {
    let tmp = tempdir::TempDir::new("app-tui-session").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one")]);
    let session = resolve_session(&dir, Some("missing")).unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert!(handle.history().is_empty(), "fresh session has no history");
    assert_eq!(handle.prompt("hello").await.unwrap(), "one");
    handle.shutdown().await;

    assert!(!dir.join("missing.jsonl").exists());
    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    assert_eq!(files.len(), 1);
    let stem = files[0].strip_suffix(".jsonl").unwrap();
    let parsed = uuid::Uuid::parse_str(stem).unwrap();
    assert_eq!(parsed.get_version_num(), 7);
}

#[tokio::test]
async fn fresh_session_without_messages_has_no_id_and_no_file() {
    let tmp = tempdir::TempDir::new("app-runtime-fresh").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![]);
    let session = resolve_session(&dir, None).unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert!(
        handle.session_id().is_none(),
        "empty session must not expose an id"
    );
    handle.shutdown().await;

    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert!(
        files.is_empty(),
        "no jsonl should be created for an empty session"
    );
}

#[tokio::test]
async fn clear_without_new_messages_has_no_id_and_no_file() {
    let tmp = tempdir::TempDir::new("app-runtime-clear-empty").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");

    // `/clear` switches to a fresh empty session; nothing written after.
    let mut rx = handle.subscribe();
    handle.send(AppCmd::UserInput("/clear".into())).unwrap();
    while let Some(ev) = rx.recv().await {
        if let AppEvent::Idle { .. } = ev {
            break;
        }
    }
    assert!(
        handle.session_id().is_none(),
        "cleared session has no content yet"
    );
    handle.shutdown().await;

    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    assert_eq!(files, ["s1.jsonl"]);
}

#[tokio::test]
async fn spawn_session_without_id_creates_uuid() {
    let tmp = tempdir::TempDir::new("app-tui-session").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one")]);
    let session = resolve_session(&dir, None).unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("hi").await.unwrap(), "one");
    let sid = handle.session_id().expect("session id present");
    assert!(uuid::Uuid::parse_str(&sid).is_ok());
    handle.shutdown().await;

    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    assert_eq!(files.len(), 1);
    let stem = files[0].strip_suffix(".jsonl").unwrap();
    let parsed = uuid::Uuid::parse_str(stem).unwrap();
    assert_eq!(parsed.get_version_num(), 7);
    assert_eq!(sid, stem);
}

#[tokio::test]
async fn spawn_session_resumes_existing_id() {
    let tmp = tempdir::TempDir::new("app-tui-session").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock1 = MockProvider::new(vec![text_response("one")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock1), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    handle.shutdown().await;

    let mock2 = MockProvider::new(vec![text_response("two")]);
    let session = resolve_session(&dir, Some("s1")).unwrap();
    let handle = spawn_app_session(&app, Box::new(mock2), session).await;
    assert_eq!(handle.session_id().as_deref(), Some("s1"));
    let resumed = handle.history();
    assert_eq!(resumed.iter().filter(|m| m.role == Role::User).count(), 1);
    assert!(resumed.iter().any(|m| {
        m.role == Role::User
            && m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text == "first"))
    }));
    assert!(resumed.iter().any(|m| {
        m.role == Role::Assistant
            && m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text == "one"))
    }));
    assert_eq!(handle.prompt("second").await.unwrap(), "two");
    handle.shutdown().await;

    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    assert_eq!(files, ["s1.jsonl"]);
    let loaded = Session::open(&dir, "s1").unwrap().load().unwrap();
    assert_eq!(loaded.iter().filter(|m| m.role == Role::User).count(), 2);
}

#[tokio::test]
async fn cancel_during_turn_returns_idle() {
    use tokio::sync::oneshot;

    struct BlockProvider {
        release: Mutex<Option<oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl Provider for BlockProvider {
        async fn complete(&self, _req: &Request) -> Result<Response, ProviderError> {
            let rx = self.release.lock().unwrap().take();
            if let Some(rx) = rx {
                let _ = rx.await;
            }
            Ok(text_response("late"))
        }

        async fn stream(
            &self,
            _req: &Request,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
            Err(ProviderError::Api {
                status: 500,
                body: "no stream".into(),
            })
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("block".into())
        }
    }

    // stream fails → agent falls back to complete, which blocks
    // until we cancel (cancel wins the select over complete).
    let (tx, rx) = oneshot::channel();
    let provider = BlockProvider {
        release: Mutex::new(Some(rx)),
    };

    let tmp = tempdir::TempDir::new("app-runtime-cancel").unwrap();
    let app = App::new(tmp.path());
    let handle = spawn_app(&app, Box::new(provider)).await;
    let mut sub = handle.subscribe();
    handle.send(AppCmd::UserInput("block".into())).unwrap();

    // wait until the turn is in-flight, then cancel
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    handle.send(AppCmd::Cancel).unwrap();

    let mut saw_idle = false;
    let mut saw_error = false;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(AppEvent::Idle { .. })) => {
                saw_idle = true;
                break;
            }
            Ok(Some(AppEvent::Error { .. })) => saw_error = true,
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!("timeout waiting for idle after cancel"),
        }
    }
    assert!(saw_idle);
    assert!(!saw_error);
    drop(tx);
    handle.shutdown().await;
}

#[tokio::test]
async fn user_input_during_turn_is_buffered_and_runs_after() {
    use tokio::sync::oneshot;

    struct BlockOnceProvider {
        release: Mutex<Option<oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl Provider for BlockOnceProvider {
        async fn complete(&self, _req: &Request) -> Result<Response, ProviderError> {
            let rx = self.release.lock().unwrap().take();
            if let Some(rx) = rx {
                let _ = rx.await;
            }
            Ok(text_response("done"))
        }

        async fn stream(
            &self,
            _req: &Request,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
            Err(ProviderError::Api {
                status: 500,
                body: "no stream".into(),
            })
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("block-once".into())
        }
    }

    // The first turn blocks until released; a UserInput sent while it is
    // in flight must be buffered and run as its own turn afterwards.
    let (tx, rx) = oneshot::channel();
    let provider = BlockOnceProvider {
        release: Mutex::new(Some(rx)),
    };

    let tmp = tempdir::TempDir::new("app-runtime-buffer").unwrap();
    let app = App::new(tmp.path());
    let handle = spawn_app(&app, Box::new(provider)).await;
    let mut sub = handle.subscribe();

    handle.send(AppCmd::UserInput("first".into())).unwrap();
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    handle.send(AppCmd::UserInput("second".into())).unwrap();

    drop(tx);

    let mut dones = 0usize;
    let mut idles = 0usize;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(AppEvent::Agent {
                event: AgentEvent::Done { .. },
                ..
            })) => dones += 1,
            Ok(Some(AppEvent::Idle { .. })) => {
                idles += 1;
                if idles == 2 {
                    break;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!("timeout waiting for two idle events"),
        }
    }
    assert_eq!(dones, 2);
    assert_eq!(idles, 2);
    handle.shutdown().await;
}

#[tokio::test]
async fn session_persists_root_meta_and_recent_index() {
    use crate::session::{canonical_root, recent_session_id};

    let tmp = tempdir::TempDir::new("app-runtime-meta").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one")]);
    let session = resolve_session(&dir, None).unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("hi").await.unwrap(), "one");
    let sid = handle.session_id().expect("session id after content");
    handle.shutdown().await;

    // The session file's first record is the meta with the root.
    let records = Session::open(&dir, &sid).unwrap().load_records().unwrap();
    match &records[0] {
        Record::SessionMeta(meta) => {
            assert_eq!(meta.root, canonical_root(tmp.path()));
            assert!(meta.created_at > 0);
        }
        other => panic!("expected meta record, got {other:?}"),
    }

    // The recent index maps this root to the session id.
    assert_eq!(
        recent_session_id(&dir, tmp.path()).unwrap().as_deref(),
        Some(sid.as_str())
    );
}

#[tokio::test]
async fn clear_updates_recent_index_to_fresh_session() {
    use crate::session::recent_session_id;

    let tmp = tempdir::TempDir::new("app-runtime-recent-clear").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one"), text_response("fresh")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");

    let mut rx = handle.subscribe();
    handle.send(AppCmd::UserInput("/clear".into())).unwrap();
    while let Some(ev) = rx.recv().await {
        if let AppEvent::Idle { .. } = ev {
            break;
        }
    }
    assert_eq!(handle.prompt("hello").await.unwrap(), "fresh");
    let fresh = handle.session_id().expect("new session after /clear");
    handle.shutdown().await;

    assert_eq!(
        recent_session_id(&dir, tmp.path()).unwrap().as_deref(),
        Some(fresh.as_str()),
        "/clear session becomes the recent one for the root"
    );
}

fn user_texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter(|m| m.role == Role::User)
        .filter_map(|m| match &m.content[0] {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

async fn wait_rewound(
    sub: &mut mpsc::UnboundedReceiver<AppEvent>,
) -> (Option<String>, Vec<Message>, Usage) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match sub.recv().await {
                Some(AppEvent::Rewound {
                    text,
                    messages,
                    usage,
                    ..
                }) => return (text, messages, usage),
                Some(_) => {}
                None => panic!("channel closed before rewind"),
            }
        }
    })
    .await
    .expect("timeout waiting for Rewound")
}

#[tokio::test]
async fn rewind_while_idle_emits_rewound_and_drops_last_exchange() {
    let tmp = tempdir::TempDir::new("app-runtime-rewind").unwrap();
    let app = App::new(tmp.path());
    let mock = MockProvider::new(vec![
        text_response("one"),
        text_response("two"),
        text_response("three"),
    ]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert_eq!(handle.prompt("second").await.unwrap(), "two");

    let mut sub = handle.subscribe();
    handle.send(AppCmd::Rewind).unwrap();
    let (text, messages, usage) = wait_rewound(&mut sub).await;
    assert_eq!(text.as_deref(), Some("second"));
    assert_eq!(user_texts(&messages), vec!["first"]);
    // Each mocked response is 10 in / 5 out; after dropping the second
    // exchange only the first one's usage remains.
    assert_eq!((usage.input_tokens, usage.output_tokens), (10, 5));

    // A later turn runs without the rolled-back exchange in context.
    assert_eq!(handle.prompt("third").await.unwrap(), "three");
    handle.send(AppCmd::Rewind).unwrap();
    let (text, messages, usage) = wait_rewound(&mut sub).await;
    assert_eq!(text.as_deref(), Some("third"));
    assert_eq!(user_texts(&messages), vec!["first"]);
    assert_eq!((usage.input_tokens, usage.output_tokens), (10, 5));

    handle.shutdown().await;
}

#[tokio::test]
async fn rewind_with_nothing_to_remove_emits_none() {
    let tmp = tempdir::TempDir::new("app-runtime-rewind-empty").unwrap();
    let app = App::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let mut sub = handle.subscribe();
    handle.send(AppCmd::Rewind).unwrap();
    let (text, messages, usage) = wait_rewound(&mut sub).await;
    assert!(text.is_none());
    assert!(messages.is_empty());
    assert_eq!(usage, Usage::default());

    handle.shutdown().await;
}

#[tokio::test]
async fn rewind_truncates_persisted_session_file() {
    let tmp = tempdir::TempDir::new("app-runtime-rewind-session").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one"), text_response("two")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert_eq!(handle.prompt("second").await.unwrap(), "two");

    let mut sub = handle.subscribe();
    handle.send(AppCmd::Rewind).unwrap();
    wait_rewound(&mut sub).await;
    assert_eq!(
        handle.session_id().as_deref(),
        Some("s1"),
        "a rewind that leaves content keeps the session id"
    );
    handle.shutdown().await;

    let loaded = Session::open(&dir, "s1").unwrap().load().unwrap();
    assert_eq!(user_texts(&loaded), vec!["first"]);
}

#[tokio::test]
async fn rewind_all_turns_clears_session_content() {
    let tmp = tempdir::TempDir::new("app-runtime-rewind-empty").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert_eq!(handle.session_id().as_deref(), Some("s1"));

    let mut sub = handle.subscribe();
    handle.send(AppCmd::Rewind).unwrap();
    wait_rewound(&mut sub).await;
    assert!(
        handle.session_id().is_none(),
        "rewinding everything leaves nothing to resume"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn rewind_during_turn_is_queued_until_turn_ends() {
    use tokio::sync::oneshot;

    struct BlockOnceProvider {
        release: Mutex<Option<oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl Provider for BlockOnceProvider {
        async fn complete(&self, _req: &Request) -> Result<Response, ProviderError> {
            let rx = self.release.lock().unwrap().take();
            if let Some(rx) = rx {
                let _ = rx.await;
            }
            Ok(text_response("done"))
        }

        async fn stream(
            &self,
            _req: &Request,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
            Err(ProviderError::Api {
                status: 500,
                body: "no stream".into(),
            })
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("rewind-block".into())
        }
    }

    // Rewind sent while a turn is in flight must be applied after the
    // turn completes (the TUI never does this, but a stale sender or
    // boundary race must stay safe).
    let (tx, rx) = oneshot::channel();
    let provider = BlockOnceProvider {
        release: Mutex::new(Some(rx)),
    };

    let tmp = tempdir::TempDir::new("app-runtime-rewind-queue").unwrap();
    let app = App::new(tmp.path());
    let handle = spawn_app(&app, Box::new(provider)).await;
    let mut sub = handle.subscribe();

    handle.send(AppCmd::UserInput("block".into())).unwrap();
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    handle.send(AppCmd::Rewind).unwrap();
    drop(tx);

    let mut saw_idle = false;
    let mut rewound = None;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(AppEvent::Idle { .. })) => saw_idle = true,
            Ok(Some(AppEvent::Rewound {
                text,
                messages,
                usage,
                ..
            })) => {
                rewound = Some((text, messages, usage));
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!("timeout waiting for rewind after turn"),
        }
    }
    assert!(saw_idle, "Idle must arrive before the queued Rewind runs");
    let (text, messages, usage) = rewound.expect("Rewound must be emitted");
    assert_eq!(text.as_deref(), Some("block"));
    assert!(messages.is_empty(), "the whole exchange is rolled back");
    assert_eq!(usage, Usage::default());
    handle.shutdown().await;
}

#[tokio::test]
async fn set_mode_applies_during_in_flight_turn() {
    use tokio::sync::oneshot;

    struct AwaitProvider {
        entered: Mutex<Option<oneshot::Sender<()>>>,
        release: Mutex<Option<oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl Provider for AwaitProvider {
        async fn complete(&self, _req: &Request) -> Result<Response, ProviderError> {
            if let Some(tx) = self.entered.lock().unwrap().take() {
                let _ = tx.send(());
            }
            let rx = self.release.lock().unwrap().take();
            if let Some(rx) = rx {
                let _ = rx.await;
            }
            Ok(text_response("late"))
        }

        async fn stream(
            &self,
            _req: &Request,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
            Err(ProviderError::Api {
                status: 500,
                body: "no stream".into(),
            })
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("await-mode".into())
        }
    }

    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = AwaitProvider {
        entered: Mutex::new(Some(entered_tx)),
        release: Mutex::new(Some(release_rx)),
    };

    let agent = agent_from(Box::new(provider));
    assert_eq!(agent.mode(), AgentMode::Default);
    let live = agent.live_handle();
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        None,
        "default".into(),
        PathBuf::from("/tmp"),
        AppConfig::default(),
        None,
    );
    let mut sub = handle.subscribe();
    handle.send(AppCmd::UserInput("block".into())).unwrap();
    entered_rx.await.expect("turn entered complete");
    handle.send(AppCmd::SetMode(AgentMode::Plan)).unwrap();

    let mut saw_mode = false;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(AppEvent::ModeChanged { mode, .. })) => {
                assert_eq!(mode, AgentMode::Plan);
                saw_mode = true;
                break;
            }
            Ok(Some(AppEvent::Idle { .. })) => {
                panic!("Idle arrived before ModeChanged; SetMode was queued")
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!("timeout waiting for ModeChanged"),
        }
    }
    assert!(saw_mode);
    assert_eq!(
        live.lock().unwrap_or_else(|e| e.into_inner()).mode,
        AgentMode::Plan
    );
    drop(release_tx);
    handle.shutdown().await;
}

fn tool_response(id: &str, name: &str, input: serde_json::Value) -> Response {
    Response {
        id: "resp".into(),
        model: "mock".into(),
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
        }],
        stop_reason: Some(StopReason::ToolUse),
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
        }),
    }
}

fn last_jsonl_line(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn never_todo_write_session_has_no_todo_list_line() {
    let tmp = tempdir::TempDir::new("app-runtime-no-todo").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let mock = MockProvider::new(vec![text_response("one"), text_response("two")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert_eq!(handle.prompt("second").await.unwrap(), "two");
    handle.shutdown().await;

    let records = Session::open(&dir, "s1").unwrap().load_records().unwrap();
    assert!(
        !records.iter().any(|r| matches!(r, Record::TodoList { .. })),
        "never-write sessions must not grow a todo_list line"
    );
}

#[tokio::test]
async fn todo_write_appends_snapshot_without_advancing_prefix() {
    let tmp = tempdir::TempDir::new("app-runtime-todo-snap").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let mock = MockProvider::new(vec![
        tool_response("c1", "todo_write", serde_json::json!({"todos": []})),
        text_response("cleared"),
        text_response("next"),
    ]);
    let session = Session::open(&dir, "s1").unwrap();
    let path = session.path().to_path_buf();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("clear list").await.unwrap(), "cleared");
    let last = last_jsonl_line(&path);
    assert!(
        last.contains("\"type\":\"todo_list\""),
        "last line after write must be snapshot: {last}"
    );
    assert!(last.contains("\"items\":[]"), "{last}");

    assert_eq!(handle.prompt("second").await.unwrap(), "next");
    handle.shutdown().await;

    let loaded = Session::open(&dir, "s1").unwrap().load().unwrap();
    assert_eq!(user_texts(&loaded), vec!["clear list", "second"]);
}

#[tokio::test]
async fn cancel_does_not_roll_back_todos() {
    use tokio::sync::oneshot;

    struct WriteThenBlock {
        release: Mutex<Option<oneshot::Receiver<()>>>,
        step: Mutex<u8>,
    }

    #[async_trait]
    impl Provider for WriteThenBlock {
        async fn complete(&self, _req: &Request) -> Result<Response, ProviderError> {
            let n = {
                let mut step = self.step.lock().unwrap();
                let n = *step;
                *step += 1;
                n
            };
            if n == 0 {
                return Ok(tool_response(
                    "c1",
                    "todo_write",
                    serde_json::json!({
                        "todos":[{"id":"a","content":"one","status":"in_progress"}]
                    }),
                ));
            }
            let rx = self.release.lock().unwrap().take();
            if let Some(rx) = rx {
                let _ = rx.await;
            }
            Ok(text_response("late"))
        }

        async fn stream(
            &self,
            _req: &Request,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
            Err(ProviderError::Api {
                status: 500,
                body: "no stream".into(),
            })
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("todo-cancel".into())
        }
    }

    let (tx, rx) = oneshot::channel();
    let provider = WriteThenBlock {
        release: Mutex::new(Some(rx)),
        step: Mutex::new(0),
    };
    let tmp = tempdir::TempDir::new("app-runtime-todo-cancel").unwrap();
    let app = App::new(tmp.path());
    let agent = app
        .build_agent_with_provider(Box::new(provider))
        .await
        .unwrap();
    let live = agent.live_handle();
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        None,
        "default".into(),
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );
    let mut sub = handle.subscribe();
    handle.send(AppCmd::UserInput("plan".into())).unwrap();

    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(AppEvent::Agent {
                event: AgentEvent::TodoUpdated { items, .. },
                ..
            })) if !items.is_empty() => break,
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before TodoUpdated"),
            Err(_) => panic!("timeout waiting for TodoUpdated"),
        }
    }
    handle.send(AppCmd::Cancel).unwrap();
    drop(tx);
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(AppEvent::Idle { .. })) => break,
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before Idle"),
            Err(_) => panic!("timeout waiting for Idle after cancel"),
        }
    }
    assert_eq!(
        live.lock().unwrap_or_else(|e| e.into_inner()).todos.items[0].id,
        "a"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn rewind_restores_previous_todo_list() {
    let tmp = tempdir::TempDir::new("app-runtime-todo-rewind").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let mock = MockProvider::new(vec![
        tool_response(
            "c1",
            "todo_write",
            serde_json::json!({
                "todos":[{"id":"a","content":"one","status":"pending"}]
            }),
        ),
        text_response("first"),
        tool_response(
            "c2",
            "todo_write",
            serde_json::json!({
                "todos":[{"id":"b","content":"two","status":"in_progress"}]
            }),
        ),
        text_response("second"),
    ]);
    let session = Session::open(&dir, "s1").unwrap();
    let agent = app.build_agent_with_provider(Box::new(mock)).await.unwrap();
    let live = agent.live_handle();
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        Some(session),
        "default".into(),
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );
    assert_eq!(handle.prompt("t1").await.unwrap(), "first");
    assert_eq!(handle.prompt("t2").await.unwrap(), "second");
    assert_eq!(
        live.lock().unwrap_or_else(|e| e.into_inner()).todos.items[0].id,
        "b"
    );

    let mut sub = handle.subscribe();
    handle.send(AppCmd::Rewind).unwrap();
    let mut saw_todo = false;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(AppEvent::Agent {
                event: AgentEvent::TodoUpdated { items, .. },
                ..
            })) => {
                assert_eq!(items[0].id, "a");
                saw_todo = true;
            }
            Ok(Some(AppEvent::Rewound { .. })) => {
                assert!(saw_todo, "TodoUpdated must precede Rewound");
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before Rewound"),
            Err(_) => panic!("timeout waiting for rewind"),
        }
    }
    assert_eq!(
        live.lock().unwrap_or_else(|e| e.into_inner()).todos.items[0].id,
        "a"
    );
    handle.shutdown().await;

    let records = Session::open(&dir, "s1").unwrap().load_records().unwrap();
    let last_list = records.iter().rev().find_map(|r| match r {
        Record::TodoList { items, .. } => Some(items.as_slice()),
        _ => None,
    });
    assert_eq!(last_list.unwrap()[0].id, "a");
}

#[tokio::test]
async fn resume_hydrates_todos_from_snapshot() {
    let tmp = tempdir::TempDir::new("app-runtime-todo-hydrate").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let mock1 = MockProvider::new(vec![
        tool_response(
            "c1",
            "todo_write",
            serde_json::json!({
                "todos":[{"id":"keep","content":"stay","status":"pending"}]
            }),
        ),
        text_response("one"),
    ]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock1), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    handle.shutdown().await;

    let mock2 = MockProvider::new(vec![text_response("two")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock2), session).await;
    assert_eq!(handle.todos().items.len(), 1);
    assert_eq!(handle.todos().items[0].id, "keep");
    handle.shutdown().await;
}

#[tokio::test]
async fn slash_clear_does_not_copy_todos_to_new_session() {
    let tmp = tempdir::TempDir::new("app-runtime-clear-todos").unwrap();
    let app = App::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let mock = MockProvider::new(vec![
        tool_response(
            "c1",
            "todo_write",
            serde_json::json!({
                "todos":[{"id":"old","content":"gone","status":"pending"}]
            }),
        ),
        text_response("one"),
        text_response("fresh"),
    ]);
    let session = Session::open(&dir, "s1").unwrap();
    let agent = app.build_agent_with_provider(Box::new(mock)).await.unwrap();
    let live = agent.live_handle();
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        Some(session),
        "default".into(),
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert!(
        !live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .todos
            .is_empty()
    );

    handle.prompt("/clear").await.unwrap();
    assert!(
        live.lock()
            .unwrap_or_else(|e| e.into_inner())
            .todos
            .is_empty()
    );
    assert_eq!(handle.prompt("hello").await.unwrap(), "fresh");
    let sid = handle.session_id().expect("new session");
    handle.shutdown().await;

    let fresh = Session::open(&dir, &sid).unwrap().load_records().unwrap();
    assert!(
        !fresh.iter().any(|r| matches!(r, Record::TodoList { .. })),
        "new session must not inherit the old todo_list"
    );
}

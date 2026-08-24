use crate::command::AppCommand;
use crate::config::{AppConfig, ProviderConfig};
use crate::event::{AppEvent, AppEventKind, AppId};
use crate::runtime::*;
use crate::session::{Session, canonical_root};
use crate::state::{AppPhase, StateChange, StateEvent};
use crate::{App, AppBuilder};
use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream::BoxStream;
use oven_agent::{Agent, AgentEvent, AgentEventEnvelope, AgentMode, Record, TurnEvent, TurnId};
use oven_llm::{
    ContentBlock, Message, ModelId, ModelInfo, Provider, ProviderError, ProviderName, Request,
    Response, Role, Router, StopReason, StreamEvent, Usage,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

fn agent_from(provider: Box<dyn Provider>) -> Agent {
    let mut router = Router::new();
    router.register(provider);
    Agent::new(router, Vec::new())
}

async fn spawn_app(app: &AppBuilder, provider: Box<dyn Provider>) -> App {
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

async fn spawn_app_session(app: &AppBuilder, provider: Box<dyn Provider>, session: Session) -> App {
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

fn is_turn_completed(ev: &AppEvent) -> bool {
    matches!(
        ev.kind,
        AppEventKind::Agent(ref env) if matches!(env.event, AgentEvent::Turn(TurnEvent::Completed { .. }))
    )
}

fn is_turn_cancelled(ev: &AppEvent) -> bool {
    matches!(
        ev.kind,
        AppEventKind::Agent(ref env) if matches!(env.event, AgentEvent::Turn(TurnEvent::Cancelled))
    )
}

fn is_exited(ev: &AppEvent) -> bool {
    matches!(ev.kind, AppEventKind::Exited)
}

fn notification(ev: &AppEvent) -> Option<&str> {
    match &ev.kind {
        AppEventKind::Notification { text } => Some(text.as_str()),
        _ => None,
    }
}

fn is_history_changed(ev: &AppEvent) -> bool {
    matches!(
        ev.kind,
        AppEventKind::StateChanged(StateEvent {
            change: StateChange::HistoryChanged { .. },
            ..
        })
    )
}

fn is_mode_changed(ev: &AppEvent, want: oven_agent::AgentMode) -> bool {
    matches!(
        ev.kind,
        AppEventKind::StateChanged(StateEvent {
            change: StateChange::ModeChanged { mode },
            ..
        }) if mode == want
    )
}

fn turn_id_of(ev: &AppEvent) -> Option<TurnId> {
    match &ev.kind {
        AppEventKind::Agent(env) => Some(env.turn_id),
        _ => None,
    }
}

async fn wait_turn_id(sub: &mut mpsc::UnboundedReceiver<AppEvent>) -> TurnId {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match sub.recv().await {
                Some(ev) => {
                    if let AppEventKind::Agent(env) = &ev.kind
                        && matches!(env.event, AgentEvent::Turn(TurnEvent::Started))
                    {
                        return env.turn_id;
                    }
                }
                None => panic!("channel closed before TurnStarted"),
            }
        }
    })
    .await
    .expect("timeout waiting for TurnStarted")
}

async fn wait_settled(sub: &mut mpsc::UnboundedReceiver<AppEvent>) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match sub.recv().await {
                Some(ev)
                    if is_turn_completed(&ev)
                        || is_turn_cancelled(&ev)
                        || is_exited(&ev)
                        || notification(&ev).is_some() =>
                {
                    return;
                }
                Some(AppEvent {
                    kind: AppEventKind::Error { .. },
                    ..
                }) => return,
                Some(_) => {}
                None => panic!("channel closed before settle"),
            }
        }
    })
    .await
    .expect("timeout waiting for command to settle");
}

#[tokio::test]
async fn spawn_prompt_emits_done_and_idle() {
    let tmp = tempdir::TempDir::new("app-runtime").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![text_response("hello")]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let mut rx = handle.subscribe();
    assert!(
        handle.session_id().is_none(),
        "no session id without persistence"
    );
    let text = handle.prompt("hi").await.unwrap();
    assert_eq!(text, "hello");

    let mut saw_completed = false;
    while let Ok(ev) = rx.try_recv() {
        if is_turn_completed(&ev) {
            saw_completed = true;
        }
    }
    assert!(saw_completed);
    assert!(matches!(handle.state().phase, AppPhase::Idle));

    handle.shutdown().await;
}

#[tokio::test]
async fn handle_exposes_slash_commands() {
    let tmp = tempdir::TempDir::new("app-runtime-slash").unwrap();
    let app = AppBuilder::new(tmp.path());
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
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let mut rx = handle.subscribe();
    let status = handle.prompt("/plan").await.unwrap();
    assert!(status.contains("current mode: agent"));
    assert!(status.contains("0 todos"));

    let _ = handle.prompt("/plan on").await.unwrap();
    let mut saw_plan = false;
    while let Ok(ev) = rx.try_recv() {
        if is_mode_changed(&ev, AgentMode::Plan) {
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
        match &ev.kind {
            AppEventKind::Notification { text }
                if text == "model switched to mock/gpt-4o-turbo (effort: low)" =>
            {
                saw_reply = true;
            }
            AppEventKind::Agent(env)
                if matches!(env.event, AgentEvent::Turn(TurnEvent::Completed { .. })) =>
            {
                saw_done = true;
            }
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
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let mut rx = handle.subscribe();
    let out = handle.prompt("/model").await.unwrap();
    assert!(out.contains("current model"));

    let mut saw_reply = false;
    let mut saw_done = false;
    while let Ok(ev) = rx.try_recv() {
        match &ev.kind {
            AppEventKind::Notification { text } if text.contains("current model") => {
                saw_reply = true;
            }
            AppEventKind::Agent(env)
                if matches!(env.event, AgentEvent::Turn(TurnEvent::Completed { .. })) =>
            {
                saw_done = true;
            }
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
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let mut rx = handle.subscribe();
    handle
        .send(AppCommand::StartTurn {
            input: "/exit".into(),
        })
        .unwrap();

    let mut saw_exit = false;
    while let Some(ev) = rx.recv().await {
        if is_exited(&ev) {
            saw_exit = true;
            break;
        }
    }
    assert!(saw_exit);
    handle.shutdown().await;
}

#[tokio::test]
async fn slash_clear_does_not_call_provider() {
    let tmp = tempdir::TempDir::new("app-runtime-clear-noprovider").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;
    assert_eq!(handle.prompt("/clear").await.unwrap(), "history cleared");
    handle.shutdown().await;
}

#[tokio::test]
async fn slash_clear_emits_history_cleared_and_resets_usage() {
    let tmp = tempdir::TempDir::new("app-runtime-clear-events").unwrap();
    let app = AppBuilder::new(tmp.path());
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
        match &ev.kind {
            AppEventKind::StateChanged(StateEvent {
                change: StateChange::HistoryChanged { .. },
                ..
            }) => saw_cleared = true,
            AppEventKind::StateChanged(StateEvent {
                change: StateChange::TodosChanged { todos },
                ..
            }) if todos.is_empty() => saw_todos_cleared = true,
            AppEventKind::StateChanged(StateEvent {
                change: StateChange::UsageChanged { usage },
                ..
            }) => done_usage = Some(*usage),
            _ => {}
        }
    }
    assert!(saw_cleared);
    assert!(saw_todos_cleared);
    let usage = done_usage.expect("usage after /clear");
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.output_tokens, 0);
    handle.shutdown().await;
}

#[tokio::test]
async fn slash_exit_returns_goodbye() {
    let tmp = tempdir::TempDir::new("app-runtime-exit-prompt").unwrap();
    let app = AppBuilder::new(tmp.path());
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
        tmp.path().to_path_buf(),
        AppConfig::default(),
        Some(cfg_path.clone()),
    );

    let mut rx = handle.subscribe();
    handle
        .send(AppCommand::StartTurn {
            input: "/setup name=deepseek api_key=sk-test".into(),
        })
        .unwrap();
    let mut out = String::new();
    loop {
        match rx.recv().await {
            Some(ev) => {
                if let Some(text) = notification(&ev) {
                    out.push_str(text);
                    break;
                }
                if matches!(ev.kind, AppEventKind::Error { .. }) {
                    panic!("setup failed: {ev:?}");
                }
            }
            None => panic!("channel closed before notify"),
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
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );

    let mut rx = handle.subscribe();
    handle
        .send(AppCommand::StartTurn {
            input: "/setup name=deepseek api_key=sk-test".into(),
        })
        .unwrap();
    loop {
        match rx.recv().await {
            Some(ev)
                if notification(&ev).is_some() || matches!(ev.kind, AppEventKind::Error { .. }) =>
            {
                break;
            }
            Some(_) => {}
            None => panic!("channel closed before notify"),
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
        .send(AppCommand::StartTurn {
            input: "/setup name=deepseek api_key=sk-test".into(),
        })
        .unwrap();
    loop {
        match rx.recv().await {
            Some(ev)
                if notification(&ev).is_some() || matches!(ev.kind, AppEventKind::Error { .. }) =>
            {
                break;
            }
            Some(_) => {}
            None => panic!("channel closed before notify"),
        }
    }
    let current = handle.prompt("/model").await.unwrap();
    assert!(current.contains("reasoning effort: high"));
    let saved = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(!saved.contains("reasoning_effort"));
    handle.shutdown().await;
}

#[tokio::test]
async fn open_session_without_api_key_starts() {
    let tmp = tempdir::TempDir::new("app-first-run").unwrap();
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let app = AppBuilder::new(tmp.path());
    let handle = app.open_session_in(&dir, None).await.unwrap();
    handle.shutdown().await;
}

#[tokio::test]
async fn spawn_without_api_key_still_errors() {
    let tmp = tempdir::TempDir::new("app-headless-no-key").unwrap();
    let app = AppBuilder::new(tmp.path());
    if !app.config().provider.needs_setup() {
        return;
    }
    let err = match app.open().await {
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
    let app = AppBuilder::new(tmp.path());
    let handle = spawn_app(&app, Box::new(MockProvider::new(vec![]))).await;
    let err = handle.prompt("/setup kind=chat").await.unwrap_err();
    assert!(err.to_string().contains("kind is no longer used"));
    handle.shutdown().await;
}

#[tokio::test]
async fn spawn_applies_configured_reasoning_effort() {
    let tmp = tempdir::TempDir::new("app-spawn-effort").unwrap();
    let app = AppBuilder::new(tmp.path()).with_config(AppConfig {
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
    let app = AppBuilder::new(tmp.path());
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
    let app = AppBuilder::new(tmp.path());
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
    handle.send(AppCommand::Rewind).unwrap();
    wait_rewound(&mut sub).await;
    assert_eq!(user_texts(&handle.history()), vec!["first"]);
    assert_eq!(
        (
            handle.total_usage().input_tokens,
            handle.total_usage().output_tokens
        ),
        (10, 5)
    );

    assert_eq!(handle.prompt("third").await.unwrap(), "three");
    handle.send(AppCommand::Rewind).unwrap();
    wait_rewound(&mut sub).await;
    assert_eq!(
        (
            handle.total_usage().input_tokens,
            handle.total_usage().output_tokens
        ),
        (10, 5)
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn slash_clear_starts_new_session() {
    let tmp = tempdir::TempDir::new("app-runtime-clear").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    // Turn 1 persists a message so the file is non-empty.
    let mock = MockProvider::new(vec![text_response("one"), text_response("fresh")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");

    // `/clear` switches the runtime to a fresh uuid v7 session.
    let mut rx = handle.subscribe();
    handle
        .send(AppCommand::StartTurn {
            input: "/clear".into(),
        })
        .unwrap();
    wait_settled(&mut rx).await;

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
async fn open_session_creates_uuid_when_id_missing() {
    let tmp = tempdir::TempDir::new("app-tui-session").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one")]);
    let session = Session::resolve(&dir, Some("missing")).unwrap();
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
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![]);
    let session = Session::resolve(&dir, None).unwrap();
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
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");

    // `/clear` switches to a fresh empty session; nothing written after.
    let mut rx = handle.subscribe();
    handle
        .send(AppCommand::StartTurn {
            input: "/clear".into(),
        })
        .unwrap();
    wait_settled(&mut rx).await;
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
async fn open_session_without_id_creates_uuid() {
    let tmp = tempdir::TempDir::new("app-tui-session").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one")]);
    let session = Session::resolve(&dir, None).unwrap();
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
async fn open_session_resumes_existing_id() {
    let tmp = tempdir::TempDir::new("app-tui-session").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock1 = MockProvider::new(vec![text_response("one")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock1), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    handle.shutdown().await;

    let mock2 = MockProvider::new(vec![text_response("two")]);
    let session = Session::resolve(&dir, Some("s1")).unwrap();
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
    let app = AppBuilder::new(tmp.path());
    let handle = spawn_app(&app, Box::new(provider)).await;
    let mut sub = handle.subscribe();
    handle
        .send(AppCommand::StartTurn {
            input: "block".into(),
        })
        .unwrap();
    let turn_id = wait_turn_id(&mut sub).await;
    handle.send(AppCommand::Cancel { turn_id }).unwrap();

    let mut saw_cancelled = false;
    let mut saw_error = false;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(ev)) if is_turn_cancelled(&ev) => {
                saw_cancelled = true;
                break;
            }
            Ok(Some(AppEvent {
                kind: AppEventKind::Error { .. },
                ..
            })) => saw_error = true,
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!("timeout waiting for cancel"),
        }
    }
    assert!(saw_cancelled);
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
    let app = AppBuilder::new(tmp.path());
    let handle = spawn_app(&app, Box::new(provider)).await;
    let mut sub = handle.subscribe();

    handle
        .send(AppCommand::StartTurn {
            input: "first".into(),
        })
        .unwrap();
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    handle
        .send(AppCommand::StartTurn {
            input: "second".into(),
        })
        .unwrap();

    drop(tx);

    let mut completed = 0usize;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(ev)) if is_turn_completed(&ev) => {
                completed += 1;
                if completed == 2 {
                    break;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!("timeout waiting for two completed turns"),
        }
    }
    assert_eq!(completed, 2);
    handle.shutdown().await;
}

#[tokio::test]
async fn session_persists_root_meta_and_recent_index() {
    use crate::session::{canonical_root, recent_session_id};

    let tmp = tempdir::TempDir::new("app-runtime-meta").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one")]);
    let session = Session::resolve(&dir, None).unwrap();
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
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one"), text_response("fresh")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");

    let mut rx = handle.subscribe();
    handle
        .send(AppCommand::StartTurn {
            input: "/clear".into(),
        })
        .unwrap();
    wait_settled(&mut rx).await;
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

async fn wait_rewound(sub: &mut mpsc::UnboundedReceiver<AppEvent>) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match sub.recv().await {
                Some(ev) if is_history_changed(&ev) => return,
                Some(_) => {}
                None => panic!("channel closed before rewind"),
            }
        }
    })
    .await
    .expect("timeout waiting for HistoryChanged")
}

#[tokio::test]
async fn rewind_while_idle_emits_rewound_and_drops_last_exchange() {
    let tmp = tempdir::TempDir::new("app-runtime-rewind").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![
        text_response("one"),
        text_response("two"),
        text_response("three"),
    ]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert_eq!(handle.prompt("second").await.unwrap(), "two");

    let mut sub = handle.subscribe();
    handle.send(AppCommand::Rewind).unwrap();
    wait_rewound(&mut sub).await;
    assert_eq!(user_texts(&handle.history()), vec!["first"]);
    assert_eq!(
        (
            handle.total_usage().input_tokens,
            handle.total_usage().output_tokens
        ),
        (10, 5)
    );

    assert_eq!(handle.prompt("third").await.unwrap(), "three");
    handle.send(AppCommand::Rewind).unwrap();
    wait_rewound(&mut sub).await;
    assert_eq!(user_texts(&handle.history()), vec!["first"]);
    assert_eq!(
        (
            handle.total_usage().input_tokens,
            handle.total_usage().output_tokens
        ),
        (10, 5)
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn rewind_with_nothing_to_remove_emits_none() {
    let tmp = tempdir::TempDir::new("app-runtime-rewind-empty").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let mut sub = handle.subscribe();
    handle.send(AppCommand::Rewind).unwrap();
    wait_rewound(&mut sub).await;
    assert!(handle.history().is_empty());
    assert_eq!(handle.total_usage(), Usage::default());

    handle.shutdown().await;
}

#[tokio::test]
async fn rewind_truncates_persisted_session_file() {
    let tmp = tempdir::TempDir::new("app-runtime-rewind-session").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one"), text_response("two")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert_eq!(handle.prompt("second").await.unwrap(), "two");

    let mut sub = handle.subscribe();
    handle.send(AppCommand::Rewind).unwrap();
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
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one")]);
    let session = Session::open(&dir, "s1").unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert_eq!(handle.session_id().as_deref(), Some("s1"));

    let mut sub = handle.subscribe();
    handle.send(AppCommand::Rewind).unwrap();
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
    let app = AppBuilder::new(tmp.path());
    let handle = spawn_app(&app, Box::new(provider)).await;
    let mut sub = handle.subscribe();

    handle
        .send(AppCommand::StartTurn {
            input: "block".into(),
        })
        .unwrap();
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    handle.send(AppCommand::Rewind).unwrap();
    drop(tx);

    let mut saw_completed = false;
    let mut rewound = false;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(ev)) if is_turn_completed(&ev) => saw_completed = true,
            Ok(Some(ev)) if is_history_changed(&ev) => {
                rewound = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!("timeout waiting for rewind after turn"),
        }
    }
    assert!(
        saw_completed,
        "TurnCompleted must arrive before the queued Rewind runs"
    );
    assert!(rewound, "HistoryChanged must be emitted");
    assert!(
        handle.history().is_empty(),
        "the whole exchange is rolled back"
    );
    assert_eq!(handle.total_usage(), Usage::default());
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
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        None,
        PathBuf::from("/tmp"),
        AppConfig::default(),
        None,
    );
    let mut sub = handle.subscribe();
    handle
        .send(AppCommand::StartTurn {
            input: "block".into(),
        })
        .unwrap();
    entered_rx.await.expect("turn entered complete");
    handle
        .send(AppCommand::SetMode {
            mode: AgentMode::Plan,
        })
        .unwrap();

    let mut saw_mode = false;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(ev)) if is_mode_changed(&ev, AgentMode::Plan) => {
                saw_mode = true;
                break;
            }
            Ok(Some(ev)) if is_turn_completed(&ev) => {
                panic!("TurnCompleted arrived before ModeChanged; SetMode was queued")
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!("timeout waiting for ModeChanged"),
        }
    }
    assert!(saw_mode);
    assert_eq!(handle.state().mode, AgentMode::Plan);
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
    let app = AppBuilder::new(tmp.path());
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
    let app = AppBuilder::new(tmp.path());
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
    let app = AppBuilder::new(tmp.path());
    let agent = app
        .build_agent_with_provider(Box::new(provider))
        .await
        .unwrap();
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        None,
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );
    let mut sub = handle.subscribe();
    handle
        .send(AppCommand::StartTurn {
            input: "plan".into(),
        })
        .unwrap();

    let mut turn_id = None;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(AppEvent {
                kind:
                    AppEventKind::StateChanged(StateEvent {
                        change: StateChange::TodosChanged { todos },
                        ..
                    }),
                ..
            })) if !todos.is_empty() => break,
            Ok(Some(ev)) => {
                if turn_id.is_none() {
                    turn_id = turn_id_of(&ev);
                }
            }
            Ok(None) => panic!("channel closed before TodosChanged"),
            Err(_) => panic!("timeout waiting for TodosChanged"),
        }
    }
    let turn_id = turn_id.unwrap_or_else(|| match handle.state().phase {
        AppPhase::Running { turn_id } | AppPhase::Cancelling { turn_id } => turn_id,
        _ => panic!("expected a running turn"),
    });
    handle.send(AppCommand::Cancel { turn_id }).unwrap();
    drop(tx);
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(ev)) if is_turn_cancelled(&ev) => break,
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before TurnCancelled"),
            Err(_) => panic!("timeout waiting for cancel"),
        }
    }
    assert_eq!(handle.todos().items[0].id, "a");
    handle.shutdown().await;
}

#[tokio::test]
async fn rewind_restores_previous_todo_list() {
    let tmp = tempdir::TempDir::new("app-runtime-todo-rewind").unwrap();
    let app = AppBuilder::new(tmp.path());
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
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        Some(session),
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );
    assert_eq!(handle.prompt("t1").await.unwrap(), "first");
    assert_eq!(handle.prompt("t2").await.unwrap(), "second");
    assert_eq!(handle.todos().items[0].id, "b");

    let mut sub = handle.subscribe();
    handle.send(AppCommand::Rewind).unwrap();
    let mut saw_todo = false;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(AppEvent {
                kind:
                    AppEventKind::StateChanged(StateEvent {
                        change: StateChange::TodosChanged { todos },
                        ..
                    }),
                ..
            })) => {
                assert_eq!(todos.items[0].id, "a");
                saw_todo = true;
            }
            Ok(Some(ev)) if is_history_changed(&ev) => {
                assert!(saw_todo, "TodosChanged must precede HistoryChanged");
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before HistoryChanged"),
            Err(_) => panic!("timeout waiting for rewind"),
        }
    }
    assert_eq!(handle.todos().items[0].id, "a");
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
    let app = AppBuilder::new(tmp.path());
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
    let app = AppBuilder::new(tmp.path());
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
    let handle = spawn_runtime(
        AppId::next(),
        agent,
        Some(session),
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert!(!handle.todos().is_empty());

    handle.prompt("/clear").await.unwrap();
    assert!(handle.todos().is_empty());
    assert_eq!(handle.prompt("hello").await.unwrap(), "fresh");
    let sid = handle.session_id().expect("new session");
    handle.shutdown().await;

    let fresh = Session::open(&dir, &sid).unwrap().load_records().unwrap();
    assert!(
        !fresh.iter().any(|r| matches!(r, Record::TodoList { .. })),
        "new session must not inherit the old todo_list"
    );
}

fn agent_envelopes(events: &[AppEvent]) -> Vec<&AgentEventEnvelope> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            AppEventKind::Agent(env) => Some(env),
            _ => None,
        })
        .collect()
}

fn assert_one_started_one_terminal(envs: &[&AgentEventEnvelope]) {
    assert!(
        matches!(
            envs.first().map(|e| &e.event),
            Some(AgentEvent::Turn(TurnEvent::Started))
        ),
        "first agent event must be TurnStarted: {envs:?}"
    );
    let started = envs
        .iter()
        .filter(|e| matches!(e.event, AgentEvent::Turn(TurnEvent::Started)))
        .count();
    let terminal = envs
        .iter()
        .filter(|e| {
            matches!(
                e.event,
                AgentEvent::Turn(
                    TurnEvent::Completed { .. } | TurnEvent::Cancelled | TurnEvent::Failed { .. }
                )
            )
        })
        .count();
    assert_eq!(started, 1, "exactly one Started");
    assert_eq!(terminal, 1, "exactly one terminal");
    assert!(
        matches!(
            envs.last().map(|e| &e.event),
            Some(AgentEvent::Turn(
                TurnEvent::Completed { .. } | TurnEvent::Cancelled | TurnEvent::Failed { .. }
            ))
        ),
        "last agent event must be terminal"
    );
    let turn_id = envs[0].turn_id;
    assert!(
        envs.iter().all(|e| e.turn_id == turn_id),
        "all agent events must belong to the active turn"
    );
}

#[tokio::test]
async fn successful_turn_lifecycle_matches_invariants() {
    let tmp = tempdir::TempDir::new("app-runtime-lifecycle").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![text_response("hello")]);
    let handle = spawn_app(&app, Box::new(mock)).await;
    assert!(handle.state().phase.is_idle());

    let mut rx = handle.subscribe();
    assert_eq!(handle.prompt("hi").await.unwrap(), "hello");
    assert!(handle.state().phase.is_idle());

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    let envs = agent_envelopes(&events);
    assert_one_started_one_terminal(&envs);
    assert!(matches!(
        envs.last().map(|e| &e.event),
        Some(AgentEvent::Turn(TurnEvent::Completed { .. }))
    ));
    handle.shutdown().await;
}

#[tokio::test]
async fn cancelled_turn_lifecycle_matches_invariants() {
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
            ProviderName::Custom("block-lifecycle".into())
        }
    }

    let (tx, rx) = oneshot::channel();
    let provider = BlockProvider {
        release: Mutex::new(Some(rx)),
    };
    let tmp = tempdir::TempDir::new("app-runtime-cancel-lifecycle").unwrap();
    let app = AppBuilder::new(tmp.path());
    let handle = spawn_app(&app, Box::new(provider)).await;
    let mut sub = handle.subscribe();
    handle
        .send(AppCommand::StartTurn {
            input: "block".into(),
        })
        .unwrap();

    let mut events = Vec::new();
    let mut cancelled = false;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(ev)) => {
                if !cancelled
                    && let AppEventKind::Agent(env) = &ev.kind
                    && matches!(env.event, AgentEvent::Turn(TurnEvent::Started))
                {
                    assert!(matches!(
                        handle.state().phase,
                        AppPhase::Running { turn_id } if turn_id == env.turn_id
                    ));
                    handle
                        .send(AppCommand::Cancel {
                            turn_id: env.turn_id,
                        })
                        .unwrap();
                    cancelled = true;
                }
                let done = is_turn_cancelled(&ev);
                events.push(ev);
                if done {
                    break;
                }
            }
            Ok(None) => panic!("channel closed before TurnCancelled"),
            Err(_) => panic!("timeout waiting for TurnCancelled"),
        }
    }
    drop(tx);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if handle.state().phase.is_idle() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timeout waiting for Idle phase");
    let envs = agent_envelopes(&events);
    assert_one_started_one_terminal(&envs);
    assert!(matches!(
        envs.last().map(|e| &e.event),
        Some(AgentEvent::Turn(TurnEvent::Cancelled))
    ));
    handle.shutdown().await;
}

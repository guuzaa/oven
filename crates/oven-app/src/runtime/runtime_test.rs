use crate::capabilities::subagent::{SubagentParts, Subagents};
use crate::core::config::{AppConfig, ProviderConfig, ProviderSelection};
use crate::core::event::{AppEvent, AppEventKind, AppId, CompactionEvent, EventBus, ShellEvent};
use crate::core::session::{Session, canonical_root};
use crate::core::state::{AppPhase, AppState, HistoryChangeReason};
use crate::memory::{
    AMBIGUOUS_MEMORY, DESCRIPTION_LABEL, KIND_LABEL, MEMORY_DISABLED, NO_MEMORIES, REMOVED_MEMORY,
    SOURCE_LABEL,
};
use crate::{App, AppBuilder, NodeStatus};
use crate::{LocalShell, runtime::*};
use oven_mem::NOT_FOUND;
use std::borrow::Borrow;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::oneshot;

use async_trait::async_trait;
use futures::stream::BoxStream;
use oven_agent::{
    Agent, AgentEvent, AgentEventEnvelope, AgentId, AgentMode, Record, TurnEvent, TurnId,
};
use oven_llm::{
    ContentBlock, Message, ModelId, ModelInfo, Provider, ProviderError, ProviderName, Request,
    Response, Role, Router, StopReason, StreamEvent, Usage,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

fn agent_from(provider: Box<dyn Provider>) -> AppAgents {
    let mut router = Router::new();
    router.register(provider);
    agents_from(Agent::new(router, Vec::new()))
}

/// Wire a bare agent onto the app bus and an empty subagent supervisor:
/// these tests drive the runtime, not delegation.
fn agents_from(agent: Agent) -> AppAgents {
    let events = EventBus::new();
    let (wake, wake_rx) = mpsc::unbounded_channel();
    let subagents = Subagents::new(SubagentParts {
        parent: agent.id(),
        router: agent.router_handle(),
        roles: Vec::new(),
        max_concurrent: 1,
        max_iters: 1,
        events: events.clone(),
        wake: wake.clone(),
    });
    drop(wake);
    AppAgents {
        main: agent,
        subagents,
        events,
        wake_rx,
        memory: None,
        session: None,
    }
}

async fn spawn_app(app: &AppBuilder, provider: Box<dyn Provider>) -> App {
    let agents = app.build_agent_with_provider(provider).await.unwrap();
    spawn_runtime(
        AppId::next(),
        agents,
        None,
        app.root().to_path_buf(),
        app.config().clone(),
        None,
    )
}

async fn spawn_app_session(app: &AppBuilder, provider: Box<dyn Provider>, session: Session) -> App {
    let prior = session.load_records().await.unwrap();
    let sessions = crate::core::session::SessionStore::new(session.clone(), app.root(), false);
    let mut agents = app
        .build_agent_with_provider_session(provider, Some(sessions))
        .await
        .unwrap();
    let records: Vec<_> = prior
        .iter()
        .filter(|r| !matches!(r, Record::Message { message, .. } if message.role == Role::System))
        .cloned()
        .collect();
    agents.main.restore_history(records);
    hydrate_session(&mut agents.main, &prior);
    agents.main.ensure_session_meta(canonical_root(app.root()));
    spawn_runtime(
        AppId::next(),
        agents,
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

fn history(handle: &App) -> Vec<Arc<Message>> {
    handle
        .history_timed_shared()
        .into_iter()
        .map(|(message, _, _)| message)
        .collect()
}

fn timed_thinking_ms(handle: &App) -> Vec<Option<u64>> {
    handle
        .history_timed_shared()
        .into_iter()
        .map(|(_, _, thinking_ms)| thinking_ms)
        .collect()
}

fn thinking_response(thinking: &str, text: &str) -> Response {
    let mut response = text_response(text);
    response.content.insert(0, ContentBlock::thinking(thinking));
    response
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

/// Mock provider that also advertises a model catalog entry with a context
/// window, so context-based paths (auto-compaction, ctx state) activate.
struct WindowedProvider {
    inner: MockProvider,
    info: ModelInfo,
}

impl WindowedProvider {
    fn new(context_window: u32, responses: Vec<Response>) -> Self {
        let mut info = ModelInfo::minimal("default", ProviderName::Custom("mock".into()));
        info.context_window = context_window;
        Self {
            inner: MockProvider::new(responses),
            info,
        }
    }
}

#[async_trait]
impl Provider for WindowedProvider {
    async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
        self.inner.complete(req).await
    }

    async fn stream(
        &self,
        req: &Request,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        self.inner.stream(req).await
    }

    fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
        Some(&self.info)
    }

    fn provider_name(&self) -> ProviderName {
        ProviderName::Custom("mock".into())
    }
}

fn response_with_usage(text: &str, input: u32, output: u32) -> Response {
    let mut response = text_response(text);
    response.usage = Some(Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: 0,
        reasoning_tokens: 0,
    });
    response
}

fn compaction_of(ev: &AppEvent) -> Option<&CompactionEvent> {
    match &ev.kind {
        AppEventKind::Compaction(event) => Some(event),
        _ => None,
    }
}

fn turn_started(ev: &AppEvent, main: AgentId) -> bool {
    matches!(
        &ev.kind,
        AppEventKind::Agent(env)
            if env.agent_id == main && matches!(env.event, AgentEvent::Turn(TurnEvent::Started))
    )
}

/// The driver's checklist, as the turn writes it.
fn wrote_todos(ev: &AppEvent) -> bool {
    matches!(
        &ev.kind,
        AppEventKind::Agent(env)
            if matches!(&env.event, AgentEvent::TodosChanged { todos } if !todos.is_empty())
    )
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
        AppEventKind::Agent(ref env) if matches!(env.event, AgentEvent::Turn(TurnEvent::Cancelled { .. }))
    )
}

fn is_shell_done(ev: &AppEvent) -> bool {
    matches!(
        ev.kind,
        AppEventKind::Shell(ShellEvent::Finished { .. })
            | AppEventKind::Shell(ShellEvent::Failed { .. })
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

fn history_change_reason(ev: &AppEvent) -> Option<HistoryChangeReason> {
    match &ev.kind {
        AppEventKind::HistoryChanged { reason } => Some(*reason),
        _ => None,
    }
}

fn is_history_changed(ev: &AppEvent) -> bool {
    history_change_reason(ev).is_some()
}

fn turn_id_of(ev: &AppEvent) -> Option<TurnId> {
    match &ev.kind {
        AppEventKind::Agent(env) => Some(env.turn_id),
        _ => None,
    }
}

fn settle_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(if cfg!(windows) { 15 } else { 2 })
}

async fn wait_state(handle: &App, ready: impl FnMut(&AppState) -> bool) {
    tokio::time::timeout(settle_timeout(), handle.watch_state().wait_for(ready))
        .await
        .expect("timed out waiting for the state to settle")
        .expect("the runtime went away");
}

async fn wait_for_active_subagent(handle: &App) {
    wait_state(handle, |state| {
        state.subagents.iter().any(|agent| agent.status.is_active())
    })
    .await;
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
    tokio::time::timeout(settle_timeout(), async {
        loop {
            match sub.recv().await {
                Some(ev)
                    if is_turn_completed(&ev)
                        || is_turn_cancelled(&ev)
                        || is_shell_done(&ev)
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

    let commands = handle.slash_commands();
    let names: Vec<&str> = commands.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "clear", "compact", "exit", "model", "setup", "plan", "agents", "memory"
        ]
    );
    assert!(commands.iter().all(|(_, d)| !d.is_empty()));

    handle.shutdown().await;
}

#[tokio::test]
async fn plan_slash_on_idle_switches() {
    let tmp = tempdir::TempDir::new("app-runtime-plan").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let status = handle.prompt("/plan").await.unwrap();
    assert!(status.contains("current mode: agent"));
    assert!(status.contains("0 todos"));

    let _ = handle.prompt("/plan on").await.unwrap();
    assert_eq!(handle.state().mode, AgentMode::Plan);

    let status = handle.prompt("/plan").await.unwrap();
    assert!(status.contains("current mode: plan"));

    handle.shutdown().await;
}

#[tokio::test]
async fn model_slash_switch_uses_request_model() {
    let seen = recorder();
    let mut agents = agent_from(Box::new(RecordingProvider::new(seen.clone())));
    agents.main.set_model("gpt-4o");
    let handle = spawn_runtime(
        AppId::next(),
        agents,
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
    handle.submit("/exit").unwrap();

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

    let mut cleared = None;
    while let Ok(ev) = rx.try_recv() {
        if let Some(reason) = history_change_reason(&ev) {
            cleared = Some(reason);
        }
    }
    assert_eq!(cleared, Some(HistoryChangeReason::Cleared));
    assert!(handle.todos().is_empty());
    let usage = handle.last_turn_usage();
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.output_tokens, 0);
    handle.shutdown().await;
}

const FIRST_STEP_USAGE: Usage = Usage {
    input_tokens: 40,
    output_tokens: 7,
    cache_read_tokens: 0,
    reasoning_tokens: 0,
};

const LAST_STEP_USAGE: Usage = Usage {
    input_tokens: 90,
    output_tokens: 4,
    cache_read_tokens: 5,
    reasoning_tokens: 0,
};

#[tokio::test]
async fn usage_reaches_subscribers_before_the_turn_completes() {
    let tmp = tempdir::TempDir::new("app-runtime-usage-stream").unwrap();
    std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mut first = tool_response("c1", "file_read", serde_json::json!({"path": "note.txt"}));
    first.usage = Some(FIRST_STEP_USAGE);
    let mut last = text_response("done");
    last.usage = Some(LAST_STEP_USAGE);
    let handle = spawn_app(&app, Box::new(MockProvider::new(vec![first, last]))).await;

    let mut rx = handle.subscribe();
    handle.submit("read note.txt").unwrap();
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        let completed = is_turn_completed(&ev);
        events.push(ev);
        if completed {
            break;
        }
    }

    let completed_at = events.iter().position(is_turn_completed);
    let usage_at = events.iter().position(|ev| {
        matches!(
            &ev.kind,
            AppEventKind::Agent(env)
                if matches!(env.event, AgentEvent::Usage { usage } if usage == FIRST_STEP_USAGE)
        )
    });
    assert!(
        usage_at < completed_at,
        "the first step's usage must be reported mid-turn: {usage_at:?} vs {completed_at:?}"
    );

    assert_eq!(handle.last_turn_usage(), LAST_STEP_USAGE);

    handle.shutdown().await;
}

#[tokio::test]
async fn slash_compact_replaces_history_and_switches_session() {
    let tmp = tempdir::TempDir::new("app-runtime-compact").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let mock = MockProvider::new(vec![text_response("one"), text_response("the summary")]);
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;

    assert_eq!(handle.prompt("hello").await.unwrap(), "one");
    assert_eq!(handle.session_id().as_deref(), Some("s1"));

    let mut rx = handle.subscribe();
    let out = handle.prompt("/compact").await.unwrap();
    assert_eq!(out, "context compacted: 10 \u{2192} 5 tokens");

    let mut saw_started = false;
    let mut completed = None;
    let mut compacted = None;
    while let Ok(ev) = rx.try_recv() {
        if let Some(reason) = history_change_reason(&ev) {
            compacted = Some(reason);
        }
        match compaction_of(&ev) {
            Some(CompactionEvent::Started) => saw_started = true,
            Some(CompactionEvent::Completed {
                before_tokens,
                after_tokens,
            }) => completed = Some((*before_tokens, *after_tokens)),
            _ => {}
        }
    }
    assert!(saw_started);
    assert_eq!(completed, Some((10, 5)));
    assert_eq!(compacted, Some(HistoryChangeReason::Compacted));

    let history = history(&handle);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].role, Role::User);

    let new_id = handle.session_id().expect("session id after compact");
    assert_ne!(new_id, "s1");
    let records = Session::open(&dir, &new_id)
        .await
        .unwrap()
        .load_records()
        .await
        .unwrap();
    assert!(records.iter().any(|r| matches!(
        r,
        Record::Message { message, .. } if message.role == Role::User
    )));
    handle.shutdown().await;
}

#[tokio::test]
async fn slash_compact_on_empty_history_notifies_without_provider_call() {
    let tmp = tempdir::TempDir::new("app-runtime-compact-empty").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;
    assert_eq!(
        handle.prompt("/compact").await.unwrap(),
        "nothing to compact"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn failed_compact_keeps_history_and_reports_error() {
    let tmp = tempdir::TempDir::new("app-runtime-compact-fail").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![text_response("one")]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    assert_eq!(handle.prompt("hello").await.unwrap(), "one");
    let err = handle.prompt("/compact").await.unwrap_err();
    assert!(err.to_string().contains("compact failed"), "got: {err}");
    assert_eq!(history(&handle).len(), 2);
    handle.shutdown().await;
}

#[tokio::test]
async fn auto_compact_triggers_when_context_exceeds_threshold() {
    let tmp = tempdir::TempDir::new("app-runtime-autocompact").unwrap();
    let app = AppBuilder::new(tmp.path());
    let provider = WindowedProvider::new(
        100,
        vec![
            response_with_usage("one", 90, 5),
            response_with_usage("the summary", 90, 5),
        ],
    );
    let handle = spawn_app(&app, Box::new(provider)).await;

    let mut rx = handle.subscribe();
    assert_eq!(handle.prompt("hello").await.unwrap(), "one");

    let completed = tokio::time::timeout(settle_timeout(), async {
        loop {
            let ev = rx.recv().await.expect("event stream open");
            if let Some(CompactionEvent::Completed {
                before_tokens,
                after_tokens,
            }) = compaction_of(&ev)
            {
                return (*before_tokens, *after_tokens);
            }
        }
    })
    .await
    .expect("auto-compaction should complete");
    assert_eq!(completed, (90, 5));
    assert_eq!(history(&handle).len(), 1);
    handle.shutdown().await;
}

#[tokio::test]
async fn auto_compact_skipped_when_window_unknown() {
    let tmp = tempdir::TempDir::new("app-runtime-autocompact-skip").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![
        response_with_usage("one", 1_000_000, 5),
        text_response("two"),
    ]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let mut rx = handle.subscribe();
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert_eq!(handle.prompt("second").await.unwrap(), "two");

    while let Ok(ev) = rx.try_recv() {
        assert!(
            compaction_of(&ev).is_none(),
            "no compaction should run without a known context window"
        );
    }
    assert_eq!(history(&handle).len(), 4);
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
    let mut agents = agent_from(Box::new(RecordingProvider::new(seen.clone())));
    agents.main.set_model("gpt-4o");
    agents
        .main
        .set_reasoning_effort(Some(oven_llm::ReasoningEffort::Low));
    let handle = spawn_runtime(
        AppId::next(),
        agents,
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
    let agents = agent_from(Box::new(MockProvider::new(vec![])));
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig::default(),
        Some(cfg_path.clone()),
    );

    let mut rx = handle.subscribe();
    handle
        .submit("/setup name=deepseek api_key=sk-test")
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
    assert!(saved.contains("active = \"deepseek\""));
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

/// `/setup` rebuilds the router from config, so a vendor configured earlier
/// has to stay registered: switching to it must still resolve afterwards.
#[tokio::test]
async fn setup_registers_without_dropping_existing_vendor() {
    let tmp = tempdir::TempDir::new("app-runtime-setup-keep").unwrap();
    let agents = agent_from(Box::new(MockProvider::new(vec![])));
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );

    wait_setup(&handle, "/setup name=xai api_key=xai-key").await;
    wait_setup(&handle, "/setup name=deepseek api_key=sk-test").await;

    let providers = handle.configured_providers();
    assert!(providers.contains(&"xai".to_string()), "{providers:?}");
    assert!(providers.contains(&"deepseek".to_string()), "{providers:?}");
    let switched = handle.prompt("/model xai/grok-4.6").await.unwrap();
    assert!(
        switched.contains("model switched to xai/grok-4.6"),
        "{switched}"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn model_slash_persists_model_and_effort() {
    let tmp = tempdir::TempDir::new("app-runtime-model-save").unwrap();
    let cfg_path = tmp.path().join("config.toml");
    let agents = agent_from(Box::new(MockProvider::new(vec![])));
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig::default(),
        Some(cfg_path.clone()),
    );

    let out = handle.prompt("/model gpt-4o-turbo high").await.unwrap();
    assert!(out.contains("model switched to mock/gpt-4o-turbo (effort: high)"));
    assert!(out.contains(cfg_path.to_str().unwrap()));

    let saved = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(saved.contains("active = \"mock\""));
    assert!(saved.contains("[providers.mock]"));
    assert!(saved.contains("model = \"gpt-4o-turbo\""));
    assert!(saved.contains("reasoning_effort = \"high\""));
    handle.shutdown().await;
}

#[tokio::test]
async fn setup_uses_target_provider_reasoning_effort() {
    let tmp = tempdir::TempDir::new("app-runtime-setup-keep-effort").unwrap();
    let cfg_path = tmp.path().join("config.toml");
    let mut agents = agent_from(Box::new(MockProvider::new(vec![])));
    agents
        .main
        .set_reasoning_effort(Some(oven_llm::ReasoningEffort::High));
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig {
            active_provider: ProviderSelection {
                name: "mock".into(),
            },
            providers: [(
                "mock".into(),
                ProviderConfig {
                    name: Some("mock".into()),
                    reasoning_effort: Some(oven_llm::ReasoningEffort::High),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..AppConfig::default()
        },
        Some(cfg_path.clone()),
    );

    let mut rx = handle.subscribe();
    handle
        .submit("/setup name=deepseek api_key=sk-test")
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
    assert!(current.contains("reasoning effort: medium"));
    let saved = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(saved.contains("reasoning_effort = \"medium\""));
    handle.shutdown().await;
}

async fn wait_setup(handle: &App, input: &str) -> String {
    let mut rx = handle.subscribe();
    handle.submit(input).unwrap();
    loop {
        match rx.recv().await {
            Some(ev) => {
                if let Some(text) = notification(&ev) {
                    return text.to_string();
                }
                if let AppEventKind::Error { message } = &ev.kind {
                    panic!("setup failed: {message}");
                }
            }
            None => panic!("channel closed before notify"),
        }
    }
}

#[tokio::test]
async fn setup_persists_multiple_vendors_and_reuses_saved_key() {
    let tmp = tempdir::TempDir::new("app-runtime-setup-multi").unwrap();
    let cfg_path = tmp.path().join("config.toml");
    let agents = agent_from(Box::new(MockProvider::new(vec![])));
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig::default(),
        Some(cfg_path.clone()),
    );

    wait_setup(&handle, "/setup name=deepseek api_key=sk-ds").await;
    wait_setup(&handle, "/setup name=xai api_key=xai-key").await;

    let saved = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(saved.contains("[providers.deepseek]"));
    assert!(saved.contains("[providers.xai]"));
    assert!(saved.contains("sk-ds"));
    assert!(saved.contains("xai-key"));
    assert_eq!(handle.provider_config().name.as_deref(), Some("xai"));

    let out = wait_setup(&handle, "/setup name=deepseek").await;
    assert!(out.contains("name=deepseek"));
    assert_eq!(handle.provider_config().name.as_deref(), Some("deepseek"));
    let saved = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(saved.contains("sk-ds"));
    assert!(saved.contains("xai-key"));

    let switched = handle.prompt("/model xai/grok-4.6").await.unwrap();
    assert!(
        switched.contains("model switched to xai/grok-4.6"),
        "{switched}"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn setup_new_vendor_without_key_errors() {
    let tmp = tempdir::TempDir::new("app-runtime-setup-need-key").unwrap();
    let agents = agent_from(Box::new(MockProvider::new(vec![])));
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );
    let err = handle.prompt("/setup name=xai").await.unwrap_err();
    assert!(
        err.to_string().contains("api_key required"),
        "unexpected error: {err}"
    );
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
    if !app.config().needs_setup() {
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
    let app = AppBuilder::new(tmp.path())
        .with_config(AppConfig {
            active_provider: ProviderSelection {
                name: "mock".into(),
            },
            providers: [(
                "mock".into(),
                ProviderConfig {
                    name: Some("mock".into()),
                    reasoning_effort: Some(oven_llm::ReasoningEffort::Medium),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..AppConfig::default()
        })
        .await;
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
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock1), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    handle.shutdown().await;

    let loaded = loaded_messages(&dir, "s1").await;
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
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock2), session).await;
    assert_eq!(handle.prompt("second").await.unwrap(), "two");
    handle.shutdown().await;

    let loaded = loaded_messages(&dir, "s1").await;
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
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock1), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert_eq!(handle.prompt("second").await.unwrap(), "two");
    assert_eq!(
        (
            handle.last_turn_usage().input_tokens,
            handle.last_turn_usage().output_tokens
        ),
        (10, 5)
    );
    handle.shutdown().await;

    // The persisted file carries one TokenUsage record per turn; the
    // cumulative sum survives the restart. The TUI status bar shows the
    // last turn's record, not the session total.
    let records = Session::open(&dir, "s1")
        .await
        .unwrap()
        .load_records()
        .await
        .unwrap();
    let persisted: Usage = records
        .iter()
        .filter_map(|r| match r {
            Record::TokenUsage { usage, .. } => Some(*usage),
            _ => None,
        })
        .fold(Usage::default(), |acc, u| acc + u);
    assert_eq!((persisted.input_tokens, persisted.output_tokens), (20, 10));
    let last_persisted = records.iter().rev().find_map(|r| match r {
        Record::TokenUsage { usage, .. } => Some(*usage),
        _ => None,
    });
    assert_eq!(
        last_persisted.map(|u| (u.input_tokens, u.output_tokens)),
        Some((10, 5))
    );

    // Second process: resume, then rewind the last exchange.
    let mock2 = MockProvider::new(vec![text_response("three")]);
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock2), session).await;
    let timed = handle.history_timed_shared();
    assert_eq!(timed.len(), history(&handle).len());
    assert!(
        timed.iter().any(|(_, ts, _)| *ts > 0),
        "resumed history keeps Record timestamps"
    );
    assert_eq!(
        (
            handle.last_turn_usage().input_tokens,
            handle.last_turn_usage().output_tokens
        ),
        (10, 5)
    );
    let mut sub = handle.subscribe();
    handle.rewind().unwrap();
    wait_rewound(&mut sub).await;
    assert_eq!(user_texts(&history(&handle)), vec!["first"]);
    assert_eq!(
        (
            handle.last_turn_usage().input_tokens,
            handle.last_turn_usage().output_tokens
        ),
        (10, 5)
    );

    assert_eq!(handle.prompt("third").await.unwrap(), "three");
    assert_eq!(
        (
            handle.last_turn_usage().input_tokens,
            handle.last_turn_usage().output_tokens
        ),
        (10, 5)
    );
    handle.rewind().unwrap();
    wait_rewound(&mut sub).await;

    handle.shutdown().await;
}

#[tokio::test]
async fn thinking_duration_survives_a_session_resume() {
    const THINKING: &str = "weighing options";
    let tmp = tempdir::TempDir::new("app-runtime-resume-thinking").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![thinking_response(THINKING, "one")]);
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert!(
        timed_thinking_ms(&handle)
            .iter()
            .flatten()
            .any(|ms| *ms > 0),
        "a streamless provider must still time its thinking"
    );
    handle.shutdown().await;

    assert!(
        Session::open(&dir, "s1")
            .await
            .unwrap()
            .load_records()
            .await
            .unwrap()
            .iter()
            .any(
                |record| matches!(record, Record::Thinking { duration_ms, .. } if *duration_ms > 0)
            ),
        "the transcript rebuilds thinking time from the persisted record"
    );

    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(MockProvider::new(vec![])), session).await;
    assert!(
        timed_thinking_ms(&handle)
            .iter()
            .flatten()
            .any(|ms| *ms > 0),
        "resumed history keeps thinking durations"
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
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");

    // `/clear` switches the runtime to a fresh uuid v7 session.
    let mut rx = handle.subscribe();
    handle.submit("/clear").unwrap();
    wait_settled(&mut rx).await;

    // Turn 3 continues in the same handle and persists to the new session.
    assert_eq!(handle.prompt("hello").await.unwrap(), "fresh");
    let sid_after_clear = handle.session_id().expect("session id present");
    assert!(uuid::Uuid::parse_str(&sid_after_clear).is_ok());
    handle.shutdown().await;

    let old = loaded_messages(&dir, "s1").await;
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
    let fresh = loaded_messages(&dir, &fresh_ids[0]).await;
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
    let session = Session::resolve(&dir, Some("missing")).await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert!(history(&handle).is_empty(), "fresh session has no history");
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
    let session = Session::resolve(&dir, None).await.unwrap();
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
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");

    // `/clear` switches to a fresh empty session; nothing written after.
    let mut rx = handle.subscribe();
    handle.submit("/clear").unwrap();
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
    let session = Session::resolve(&dir, None).await.unwrap();
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
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock1), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    handle.shutdown().await;

    let mock2 = MockProvider::new(vec![text_response("two")]);
    let session = Session::resolve(&dir, Some("s1")).await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock2), session).await;
    assert_eq!(handle.session_id().as_deref(), Some("s1"));
    let resumed = history(&handle);
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
    let loaded = loaded_messages(&dir, "s1").await;
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
    handle.submit("block").unwrap();
    let turn_id = wait_turn_id(&mut sub).await;
    handle.cancel(turn_id);

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

    handle.submit("first").unwrap();
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    handle.submit("second").unwrap();

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
async fn a_steered_chat_is_appended_when_tool_results_are_uploaded() {
    const FOLLOW_UP: &str = "also check the tests";

    struct Capture {
        seen: Arc<Mutex<Vec<Request>>>,
        release: Mutex<Option<oneshot::Receiver<()>>>,
        responses: Mutex<std::collections::VecDeque<Response>>,
    }

    #[async_trait]
    impl Provider for Capture {
        async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
            self.seen.lock().unwrap().push(req.clone());
            let rx = self.release.lock().unwrap().take();
            if let Some(rx) = rx {
                let _ = rx.await;
            }
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
                body: "no stream".into(),
            })
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("steer-capture".into())
        }
    }

    let tmp = tempdir::TempDir::new("app-runtime-steer").unwrap();
    std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (release_tx, release_rx) = oneshot::channel();
    let provider = Capture {
        seen: Arc::clone(&seen),
        release: Mutex::new(Some(release_rx)),
        responses: Mutex::new(
            vec![
                tool_response("c1", "file_read", serde_json::json!({"path": "note.txt"})),
                text_response("done"),
            ]
            .into(),
        ),
    };

    let app = AppBuilder::new(tmp.path());
    let handle = spawn_app(&app, Box::new(provider)).await;
    let mut sub = handle.subscribe();
    handle.submit("read note.txt").unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !seen.lock().unwrap().is_empty() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the first request never started");

    assert!(handle.steer(FOLLOW_UP), "a chat parks while the turn runs");
    assert!(
        !handle.steer("/clear"),
        "a slash command is not a user message"
    );
    drop(release_tx);

    let mut completed = 0usize;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(ev)) if is_turn_completed(&ev) => {
                completed += 1;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!("timeout waiting for the turn"),
        }
    }
    assert_eq!(completed, 1, "the follow-up stays inside the running turn");

    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    let uploaded = &reqs[1].messages;
    let tool_at = uploaded
        .iter()
        .rposition(|message| message.role == Role::Tool)
        .expect("tool result");
    assert!(
        matches!(&uploaded[tool_at + 1].content[0], ContentBlock::Text { text } if text == FOLLOW_UP),
        "the queued chat follows the tool result"
    );
    assert_eq!(
        user_texts(&history(&handle)),
        vec!["read note.txt", FOLLOW_UP]
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn session_persists_root_meta_and_recent_index() {
    use crate::core::session::{canonical_root, recent_session_id};

    let tmp = tempdir::TempDir::new("app-runtime-meta").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one")]);
    let session = Session::resolve(&dir, None).await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("hi").await.unwrap(), "one");
    let sid = handle.session_id().expect("session id after content");
    handle.shutdown().await;

    // The session file's first record is the meta with the root.
    let records = Session::open(&dir, &sid)
        .await
        .unwrap()
        .load_records()
        .await
        .unwrap();
    match &records[0] {
        Record::SessionMeta(meta) => {
            assert_eq!(meta.root, canonical_root(tmp.path()));
            assert!(meta.created_at > 0);
        }
        other => panic!("expected meta record, got {other:?}"),
    }

    // The recent index maps this root to the session id.
    assert_eq!(
        recent_session_id(&dir, tmp.path())
            .await
            .unwrap()
            .as_deref(),
        Some(sid.as_str())
    );
}

#[tokio::test]
async fn clear_updates_recent_index_to_fresh_session() {
    use crate::core::session::recent_session_id;

    let tmp = tempdir::TempDir::new("app-runtime-recent-clear").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one"), text_response("fresh")]);
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");

    let mut rx = handle.subscribe();
    handle.submit("/clear").unwrap();
    wait_settled(&mut rx).await;
    assert_eq!(handle.prompt("hello").await.unwrap(), "fresh");
    let fresh = handle.session_id().expect("new session after /clear");
    handle.shutdown().await;

    assert_eq!(
        recent_session_id(&dir, tmp.path())
            .await
            .unwrap()
            .as_deref(),
        Some(fresh.as_str()),
        "/clear session becomes the recent one for the root"
    );
}

async fn loaded_messages(dir: &Path, id: &str) -> Vec<Message> {
    Session::open(dir, id)
        .await
        .unwrap()
        .load_records()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|record| match record {
            Record::Message { message, .. } => Some(message),
            _ => None,
        })
        .collect()
}

fn user_texts<M: Borrow<Message>>(messages: &[M]) -> Vec<String> {
    messages
        .iter()
        .map(Borrow::borrow)
        .filter(|m| m.role == Role::User)
        .filter_map(|m| match &m.content[0] {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

async fn wait_rewound(sub: &mut mpsc::UnboundedReceiver<AppEvent>) -> HistoryChangeReason {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match sub.recv().await {
                Some(ev) => {
                    if let Some(reason) = history_change_reason(&ev) {
                        return reason;
                    }
                }
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
    handle.rewind().unwrap();
    assert_eq!(wait_rewound(&mut sub).await, HistoryChangeReason::Rewound);
    assert_eq!(user_texts(&history(&handle)), vec!["first"]);

    assert_eq!(handle.prompt("third").await.unwrap(), "three");
    handle.rewind().unwrap();
    assert_eq!(wait_rewound(&mut sub).await, HistoryChangeReason::Rewound);
    assert_eq!(user_texts(&history(&handle)), vec!["first"]);

    handle.shutdown().await;
}

#[tokio::test]
async fn rewind_with_nothing_to_remove_emits_none() {
    let tmp = tempdir::TempDir::new("app-runtime-rewind-empty").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let mut sub = handle.subscribe();
    handle.rewind().unwrap();
    wait_rewound(&mut sub).await;
    assert!(history(&handle).is_empty());
    assert_eq!(handle.last_turn_usage(), Usage::default());

    handle.shutdown().await;
}

#[tokio::test]
async fn rewind_truncates_persisted_session_file() {
    let tmp = tempdir::TempDir::new("app-runtime-rewind-session").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one"), text_response("two")]);
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert_eq!(handle.prompt("second").await.unwrap(), "two");

    let mut sub = handle.subscribe();
    handle.rewind().unwrap();
    wait_rewound(&mut sub).await;
    assert_eq!(
        handle.session_id().as_deref(),
        Some("s1"),
        "a rewind that leaves content keeps the session id"
    );
    handle.shutdown().await;

    let loaded = loaded_messages(&dir, "s1").await;
    assert_eq!(user_texts(&loaded), vec!["first"]);
}

#[tokio::test]
async fn rewind_all_turns_clears_session_content() {
    let tmp = tempdir::TempDir::new("app-runtime-rewind-empty").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let mock = MockProvider::new(vec![text_response("one")]);
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert_eq!(handle.session_id().as_deref(), Some("s1"));

    let mut sub = handle.subscribe();
    handle.rewind().unwrap();
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

    handle.submit("block").unwrap();
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    handle.rewind().unwrap();
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
        history(&handle).is_empty(),
        "the whole exchange is rolled back"
    );
    assert_eq!(handle.last_turn_usage(), Usage::default());
    handle.shutdown().await;
}

const MODEL_SWITCHED_NOTICE: &str = "model switched to mock/gpt-4o-turbo (effort: low)";

#[tokio::test]
async fn model_slash_switches_immediately_during_turn() {
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
            ProviderName::Custom("mock".into())
        }
    }

    // `/model` is validated against a `Router` snapshot and applied via
    // `TurnContext`, neither of which needs `&mut Agent`, so it can switch
    // for real while a turn is in flight instead of waiting for the turn
    // to finish.
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let provider = AwaitProvider {
        entered: Mutex::new(Some(entered_tx)),
        release: Mutex::new(Some(release_rx)),
    };

    let mut agents = agent_from(Box::new(provider));
    agents.main.set_model("gpt-4o");
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        PathBuf::from("/tmp"),
        AppConfig::default(),
        None,
    );
    let mut sub = handle.subscribe();
    handle.submit("block").unwrap();
    entered_rx.await.expect("turn entered complete");

    handle.submit("/model gpt-4o-turbo low").unwrap();

    let mut saw_switch_notice = false;
    let mut completed_before_switch = false;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(ev)) if is_turn_completed(&ev) => {
                if !saw_switch_notice {
                    completed_before_switch = true;
                }
            }
            Ok(Some(AppEvent {
                kind: AppEventKind::Notification { text },
                ..
            })) if text == MODEL_SWITCHED_NOTICE => {
                saw_switch_notice = true;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => panic!("timeout waiting for immediate /model switch"),
        }
    }

    assert!(
        saw_switch_notice,
        "expected /model to validate and switch immediately during the in-flight turn"
    );
    assert!(
        !completed_before_switch,
        "the switch must apply before the in-flight turn completes"
    );
    assert_eq!(handle.state().model, "mock/gpt-4o-turbo");

    drop(release_tx);
    handle.shutdown().await;
}

const WINDOW_PROVIDER: &str = "window-mock";
const MODEL_WITH_SMALL_WINDOW: &str = "gpt-4o";
const MODEL_WITH_LARGE_WINDOW: &str = "gpt-4o-mini";
const SMALL_WINDOW: u32 = 128_000;
const LARGE_WINDOW: u32 = 200_000;

fn windowed_model(model: &str, window: u32) -> ModelInfo {
    ModelInfo {
        context_window: window,
        ..ModelInfo::minimal(model, ProviderName::Custom(WINDOW_PROVIDER.into()))
    }
}

/// Provider that answers a tool call first and then blocks inside the second
/// response, so a test can switch models while the turn is in flight.
struct GatedWindowProvider {
    calls: AtomicUsize,
    models: Vec<ModelInfo>,
    entered: Mutex<Option<oneshot::Sender<()>>>,
    gate: tokio::sync::Mutex<Option<oneshot::Receiver<()>>>,
}

impl GatedWindowProvider {
    fn new(
        models: Vec<ModelInfo>,
        entered: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    ) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            models,
            entered: Mutex::new(Some(entered)),
            gate: tokio::sync::Mutex::new(Some(release)),
        }
    }
}

#[async_trait]
impl Provider for GatedWindowProvider {
    async fn complete(&self, _req: &Request) -> Result<Response, ProviderError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(tool_response(
                "call_1",
                "unknown_tool",
                serde_json::json!({}),
            ));
        }
        if let Some(tx) = self.entered.lock().unwrap().take() {
            let _ = tx.send(());
        }
        let mut gate = self.gate.lock().await;
        if let Some(release) = gate.take() {
            let _ = release.await;
        }
        Ok(text_response("done"))
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
        self.models.iter().find(|model| model.id == id.wire_id())
    }

    fn provider_name(&self) -> ProviderName {
        ProviderName::Custom(WINDOW_PROVIDER.into())
    }
}

#[tokio::test]
async fn mid_turn_model_switch_publishes_the_new_context_window() {
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let models = vec![
        windowed_model(MODEL_WITH_SMALL_WINDOW, SMALL_WINDOW),
        windowed_model(MODEL_WITH_LARGE_WINDOW, LARGE_WINDOW),
    ];
    let mut agents = agent_from(Box::new(GatedWindowProvider::new(
        models, entered_tx, release_rx,
    )));
    agents.main.set_model(MODEL_WITH_SMALL_WINDOW);
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        PathBuf::from("/tmp"),
        AppConfig::default(),
        None,
    );
    let mut rx = handle.subscribe();

    handle.submit("work").unwrap();
    entered_rx.await.expect("second provider response started");

    handle
        .submit(format!("/model {MODEL_WITH_LARGE_WINDOW} low"))
        .unwrap();
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
            Ok(Some(ev)) if is_turn_completed(&ev) => {
                panic!("turn completed before the mid-turn model switch")
            }
            Ok(Some(ev))
                if matches!(
                    &ev.kind,
                    AppEventKind::Notification { text } if text.contains("model switched")
                ) =>
            {
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before the mid-turn model switch"),
            Err(_) => panic!("timeout waiting for the mid-turn model switch"),
        }
    }
    let state = handle.state();
    assert!(state.phase.is_active(), "the turn is still running");
    assert_eq!(
        state.context_window,
        Some(LARGE_WINDOW),
        "the switch must publish the new model's window while the turn runs"
    );

    let _ = release_tx.send(());

    while let Some(ev) = rx.recv().await {
        if is_turn_completed(&ev) {
            break;
        }
    }
    assert_eq!(
        handle.state().context_window,
        Some(LARGE_WINDOW),
        "the switched model's window holds for the rest of the turn"
    );
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

    let agents = agent_from(Box::new(provider));
    assert_eq!(agents.main.mode(), AgentMode::Agent);
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        PathBuf::from("/tmp"),
        AppConfig::default(),
        None,
    );
    handle.submit("block").unwrap();
    entered_rx.await.expect("turn entered complete");
    handle.set_mode(AgentMode::Plan);

    let state = handle.state();
    assert!(
        state.phase.is_active(),
        "set_mode did not wait for the turn"
    );
    assert_eq!(state.mode, AgentMode::Plan);
    drop(release_tx);
    handle.shutdown().await;
}

const APPROVED_FILE: &str = "approved.txt";
const APPROVED_CONTENT: &str = "approved";

fn write_approved_command() -> &'static str {
    #[cfg(windows)]
    {
        "Set-Content -Path approved.txt -Value approved -Encoding ascii -NoNewline"
    }
    #[cfg(not(windows))]
    {
        "printf approved > approved.txt"
    }
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
            raw_arguments: None,
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

#[tokio::test]
async fn repro_ask_mode_bash_requests_approval() {
    let tmp = tempdir::TempDir::new("app-runtime-approval").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![
        tool_response(
            "c1",
            "bash",
            serde_json::json!({"command": write_approved_command()}),
        ),
        text_response("done"),
    ]);
    let handle = spawn_app(&app, Box::new(mock)).await;
    let mut rx = handle.subscribe();
    handle.set_mode(AgentMode::Ask);
    handle.submit("run it").unwrap();

    let mut request_id = None;
    while let Some(ev) = rx.recv().await {
        if let AppEventKind::Agent(env) = &ev.kind {
            if let AgentEvent::Tool(oven_agent::ToolEvent::ApprovalRequested {
                request_id: id,
                ..
            }) = &env.event
            {
                request_id = Some(*id);
                break;
            }
            if let AgentEvent::Turn(TurnEvent::Completed { .. }) = &env.event {
                panic!("turn completed without requesting approval");
            }
        }
    }
    let request_id = request_id.expect("approval requested");
    assert!(
        matches!(handle.state().phase, AppPhase::Awaiting { .. }),
        "phase: {:?}",
        handle.state().phase
    );
    handle.respond(
        request_id,
        oven_agent::UserResponse::Approval(oven_agent::ApprovalDecision::Approved),
    );

    while let Some(ev) = rx.recv().await {
        if let AppEventKind::Agent(env) = &ev.kind
            && matches!(env.event, AgentEvent::Turn(TurnEvent::Completed { .. }))
        {
            break;
        }
    }
    assert_eq!(
        std::fs::read_to_string(tmp.path().join(APPROVED_FILE)).unwrap(),
        APPROVED_CONTENT
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn answer_tool_asks_the_user_and_hands_back_their_reply() {
    const QUESTION: &str = "which database?";
    const ANSWER: &str = "postgres";
    let tmp = tempdir::TempDir::new("app-runtime-question").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![
        tool_response(
            "c1",
            "answer",
            serde_json::json!({
                "question": QUESTION,
                "options": [{ "label": ANSWER }, { "label": "sqlite" }]
            }),
        ),
        text_response("done"),
    ]);
    let handle = spawn_app(&app, Box::new(mock)).await;
    let mut rx = handle.subscribe();
    handle.submit("set up the database").unwrap();

    let mut request_id = None;
    while let Some(ev) = rx.recv().await {
        if let AppEventKind::Agent(env) = &ev.kind {
            if let AgentEvent::Tool(oven_agent::ToolEvent::QuestionAsked {
                request_id: id,
                question,
            }) = &env.event
            {
                assert_eq!(question.question, QUESTION);
                assert_eq!(question.options.len(), 2);
                request_id = Some(*id);
                break;
            }
            if let AgentEvent::Turn(TurnEvent::Completed { .. }) = &env.event {
                panic!("turn completed without asking the user");
            }
        }
    }
    let request_id = request_id.expect("question asked");
    assert!(
        matches!(handle.state().phase, AppPhase::Awaiting { .. }),
        "phase: {:?}",
        handle.state().phase
    );

    handle.respond(
        request_id,
        oven_agent::UserResponse::Answer(oven_agent::AnswerResponse::Answered {
            answer: ANSWER.into(),
        }),
    );

    while let Some(ev) = rx.recv().await {
        if let AppEventKind::Agent(env) = &ev.kind
            && matches!(env.event, AgentEvent::Turn(TurnEvent::Completed { .. }))
        {
            break;
        }
    }
    let answered = history(&handle).iter().any(|message| {
        message.content.iter().any(|block| match block {
            ContentBlock::ToolResult { content, .. } => content
                .iter()
                .any(|block| matches!(block, ContentBlock::Text { text } if text.contains(ANSWER))),
            _ => false,
        })
    });
    assert!(answered, "the user's answer must reach the model");
    handle.shutdown().await;
}

async fn spawn_loop_limit_app(tmp: &tempdir::TempDir) -> App {
    std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![
        tool_response("c1", "file_read", serde_json::json!({"path": "note.txt"})),
        tool_response("c2", "file_read", serde_json::json!({"path": "note.txt"})),
        tool_response("c3", "file_read", serde_json::json!({"path": "note.txt"})),
        text_response("done"),
    ]);
    let agents = app.build_agent_with_provider(Box::new(mock)).await.unwrap();
    spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig {
            max_iters: 2,
            ..AppConfig::default()
        },
        None,
    )
}

#[tokio::test]
async fn loop_limit_continue_completes_turn() {
    let tmp = tempdir::TempDir::new("app-runtime-loop-limit-continue").unwrap();
    let handle = spawn_loop_limit_app(&tmp).await;
    let mut rx = handle.subscribe();
    handle.submit("read it").unwrap();

    let mut request_id = None;
    while let Some(ev) = rx.recv().await {
        if let AppEventKind::Agent(env) = &ev.kind {
            match &env.event {
                AgentEvent::Turn(TurnEvent::LoopLimitReached { request_id: id, .. }) => {
                    request_id = Some(*id);
                    break;
                }
                AgentEvent::Turn(TurnEvent::Completed { .. } | TurnEvent::Failed { .. }) => {
                    panic!("turn ended without loop limit prompt");
                }
                _ => {}
            }
        }
    }
    let request_id = request_id.expect("loop limit requested");
    assert!(
        matches!(handle.state().phase, AppPhase::Awaiting { .. }),
        "phase: {:?}",
        handle.state().phase
    );
    handle.respond(
        request_id,
        oven_agent::UserResponse::LoopLimit(oven_agent::LoopLimitDecision::Continue),
    );

    while let Some(ev) = rx.recv().await {
        if let AppEventKind::Agent(env) = &ev.kind
            && matches!(env.event, AgentEvent::Turn(TurnEvent::Completed { .. }))
        {
            break;
        }
    }
    assert!(handle.state().phase.is_idle());
    handle.shutdown().await;
}

#[tokio::test]
async fn prompt_exits_loop_limit_without_hanging() {
    let tmp = tempdir::TempDir::new("app-runtime-loop-limit-prompt").unwrap();
    let handle = spawn_loop_limit_app(&tmp).await;
    let err = handle.prompt("read it").await.unwrap_err();
    assert_eq!(err.to_string(), oven_agent::MAX_ITERS_EXCEEDED);
    assert!(handle.state().phase.is_idle());
    handle.shutdown().await;
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
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    assert_eq!(handle.prompt("second").await.unwrap(), "two");
    handle.shutdown().await;

    let records = Session::open(&dir, "s1")
        .await
        .unwrap()
        .load_records()
        .await
        .unwrap();
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
    let session = Session::open(&dir, "s1").await.unwrap();
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

    let loaded = loaded_messages(&dir, "s1").await;
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
    let agents = app
        .build_agent_with_provider(Box::new(provider))
        .await
        .unwrap();
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );
    let mut sub = handle.subscribe();
    handle.submit("plan").unwrap();

    let mut turn_id = None;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(ev)) if wrote_todos(&ev) => break,
            Ok(Some(ev)) => {
                if turn_id.is_none() {
                    turn_id = turn_id_of(&ev);
                }
            }
            Ok(None) => panic!("channel closed before the checklist"),
            Err(_) => panic!("timeout waiting for the checklist"),
        }
    }
    let turn_id = turn_id.unwrap_or_else(|| match handle.state().phase {
        AppPhase::Running { turn_id } | AppPhase::Cancelling { turn_id } => turn_id,
        _ => panic!("expected a running turn"),
    });
    handle.cancel(turn_id);
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
    let session = Session::open(&dir, "s1").await.unwrap();
    let agents = app.build_agent_with_provider(Box::new(mock)).await.unwrap();
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        Some(session),
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );
    assert_eq!(handle.prompt("t1").await.unwrap(), "first");
    assert_eq!(handle.prompt("t2").await.unwrap(), "second");
    assert_eq!(handle.todos().items[0].id, "b");

    let mut sub = handle.subscribe();
    handle.rewind().unwrap();
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await {
            Ok(Some(ev)) if is_history_changed(&ev) => {
                assert_eq!(handle.todos().items[0].id, "a");
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("channel closed before HistoryChanged"),
            Err(_) => panic!("timeout waiting for rewind"),
        }
    }
    assert_eq!(handle.todos().items[0].id, "a");
    handle.shutdown().await;

    let records = Session::open(&dir, "s1")
        .await
        .unwrap()
        .load_records()
        .await
        .unwrap();
    let last_list = records.iter().rev().find_map(|r| match r {
        Record::TodoList { items, .. } => Some(items.as_slice()),
        _ => None,
    });
    assert_eq!(last_list.unwrap()[0].id, "a");
}

#[tokio::test]
async fn next_prompt_clears_finished_todos() {
    let tmp = tempdir::TempDir::new("app-runtime-todo-dismiss").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let mock = MockProvider::new(vec![
        tool_response(
            "c1",
            "todo_write",
            serde_json::json!({
                "todos":[{"id":"a","content":"one","status":"completed"}]
            }),
        ),
        text_response("done"),
        text_response("next"),
    ]);
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock), session).await;
    assert_eq!(handle.prompt("t1").await.unwrap(), "done");
    assert_eq!(handle.todos().items[0].id, "a");
    assert_eq!(handle.prompt("t2").await.unwrap(), "next");
    assert!(handle.todos().is_empty());
    handle.shutdown().await;

    let records = Session::open(&dir, "s1")
        .await
        .unwrap()
        .load_records()
        .await
        .unwrap();
    let last_list = records.iter().rev().find_map(|r| match r {
        Record::TodoList { items, .. } => Some(items.as_slice()),
        _ => None,
    });
    assert!(last_list.unwrap().is_empty());
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
    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(mock1), session).await;
    assert_eq!(handle.prompt("first").await.unwrap(), "one");
    handle.shutdown().await;

    let mock2 = MockProvider::new(vec![text_response("two")]);
    let session = Session::open(&dir, "s1").await.unwrap();
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
    let session = Session::open(&dir, "s1").await.unwrap();
    let agents = app.build_agent_with_provider(Box::new(mock)).await.unwrap();
    let handle = spawn_runtime(
        AppId::next(),
        agents,
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

    let fresh = Session::open(&dir, &sid)
        .await
        .unwrap()
        .load_records()
        .await
        .unwrap();
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
                    TurnEvent::Completed { .. }
                        | TurnEvent::Cancelled { .. }
                        | TurnEvent::Failed { .. }
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
                TurnEvent::Completed { .. }
                    | TurnEvent::Cancelled { .. }
                    | TurnEvent::Failed { .. }
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
    handle.submit("block").unwrap();

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
                    handle.cancel(env.turn_id);
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
        Some(AgentEvent::Turn(TurnEvent::Cancelled { .. }))
    ));
    handle.shutdown().await;
}

#[tokio::test]
async fn bang_shell_does_not_call_provider() {
    let tmp = tempdir::TempDir::new("app-runtime-shell").unwrap();
    let app = AppBuilder::new(tmp.path());
    let mock = MockProvider::new(vec![]);
    let handle = spawn_app(&app, Box::new(mock)).await;

    let out = handle.prompt("!echo hi").await.unwrap();
    assert!(out.contains("hi"), "{out}");
    assert!(
        handle.state().phase.is_idle(),
        "shell should return to idle"
    );

    let history = history(&handle);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].role, Role::User);
    let parsed = LocalShell::try_parse(&user_texts(&history)[0]).expect("envelope");
    assert_eq!(parsed.command, "echo hi");
    assert_eq!(parsed.exit_code, Some(0));
    assert!(parsed.output.contains("hi"), "{}", parsed.output);
    handle.shutdown().await;
}

#[tokio::test]
async fn ask_mode_bang_shell_runs_without_approval() {
    let tmp = tempdir::TempDir::new("app-runtime-shell-ask").unwrap();
    let app = AppBuilder::new(tmp.path());
    let handle = spawn_app(&app, Box::new(MockProvider::new(vec![]))).await;
    handle.set_mode(AgentMode::Ask);

    let out = handle.prompt("!echo hi").await.unwrap();
    assert!(out.contains("hi"), "{out}");
    assert!(handle.state().phase.is_idle());
    assert_eq!(handle.state().mode, AgentMode::Ask);
    let parsed = LocalShell::try_parse(&user_texts(&history(&handle))[0]).expect("envelope");
    assert_eq!(parsed.command, "echo hi");
    assert_eq!(parsed.exit_code, Some(0));
    handle.shutdown().await;
}

#[tokio::test]
async fn empty_bang_does_not_push_history() {
    let tmp = tempdir::TempDir::new("app-runtime-shell-empty").unwrap();
    let app = AppBuilder::new(tmp.path());
    let handle = spawn_app(&app, Box::new(MockProvider::new(vec![]))).await;
    let out = handle.prompt("!").await.unwrap();
    assert!(out.contains("empty shell command"), "{out}");
    assert!(history(&handle).is_empty());
    handle.shutdown().await;
}

#[tokio::test]
async fn bang_shell_nonzero_exit_is_finished_not_agent_turn() {
    let tmp = tempdir::TempDir::new("app-runtime-shell-exit").unwrap();
    let app = AppBuilder::new(tmp.path());
    let handle = spawn_app(&app, Box::new(MockProvider::new(vec![]))).await;
    let mut rx = handle.subscribe();
    handle.submit("!exit 7").unwrap();
    wait_settled(&mut rx).await;
    let parsed = LocalShell::try_parse(&user_texts(&history(&handle))[0]).unwrap();
    assert_eq!(parsed.exit_code, Some(7));
    assert!(
        parsed.output.contains("[exit code: 7]"),
        "{}",
        parsed.output
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn bang_shell_cancel_commits_cancelled_envelope() {
    let tmp = tempdir::TempDir::new("app-runtime-shell-cancel").unwrap();
    let app = AppBuilder::new(tmp.path());
    let handle = spawn_app(&app, Box::new(MockProvider::new(vec![]))).await;
    let mut rx = handle.subscribe();
    handle.submit("!sleep 60").unwrap();

    let turn_id = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match rx.recv().await {
                Some(ev) => {
                    if let AppEventKind::Shell(ShellEvent::Started { .. }) = ev.kind {
                        return handle.state().phase.turn_id().expect("running");
                    }
                }
                None => panic!("channel closed before shell started"),
            }
        }
    })
    .await
    .expect("timeout waiting for shell start");

    handle.cancel(turn_id);
    wait_settled(&mut rx).await;

    let parsed = LocalShell::try_parse(&user_texts(&history(&handle))[0]).unwrap();
    assert_eq!(parsed.command, "sleep 60");
    assert_eq!(parsed.error.as_deref(), Some("cancelled"));
    assert!(handle.state().phase.is_idle());
    handle.shutdown().await;
}

#[tokio::test]
async fn bang_shell_persists_and_rewinds() {
    let tmp = tempdir::TempDir::new("app-runtime-shell-sess").unwrap();
    let app = AppBuilder::new(tmp.path());
    let dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();

    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(MockProvider::new(vec![])), session).await;
    let _ = handle.prompt("!echo persisted").await.unwrap();
    handle.shutdown().await;

    let loaded = loaded_messages(&dir, "s1").await;
    let text = loaded
        .iter()
        .find(|m| m.role == Role::User)
        .and_then(|m| {
            m.content.iter().find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
        })
        .expect("user envelope");
    let parsed = LocalShell::try_parse(text).expect("envelope");
    assert_eq!(parsed.command, "echo persisted");

    let session = Session::open(&dir, "s1").await.unwrap();
    let handle = spawn_app_session(&app, Box::new(MockProvider::new(vec![])), session).await;
    assert_eq!(history(&handle).len(), 1);
    let mut sub = handle.subscribe();
    handle.rewind().unwrap();
    wait_rewound(&mut sub).await;
    assert!(history(&handle).is_empty());
    handle.shutdown().await;
}

#[tokio::test]
async fn bang_shell_queues_behind_agent_turn() {
    use tokio::sync::oneshot;
    struct BlockProvider {
        release: Mutex<Option<oneshot::Receiver<()>>>,
    }
    #[async_trait]
    impl Provider for BlockProvider {
        async fn complete(&self, _req: &Request) -> Result<Response, ProviderError> {
            let rx = self.release.lock().unwrap().take().unwrap();
            let _ = rx.await;
            Ok(text_response("done"))
        }
        async fn stream(
            &self,
            _req: &Request,
        ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
            Err(ProviderError::Api {
                status: 500,
                body: "unused".into(),
            })
        }
        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }
        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("block-shell-queue".into())
        }
    }

    let (tx, rx) = oneshot::channel();
    let tmp = tempdir::TempDir::new("app-runtime-shell-queue").unwrap();
    let app = AppBuilder::new(tmp.path());
    let handle = spawn_app(
        &app,
        Box::new(BlockProvider {
            release: Mutex::new(Some(rx)),
        }),
    )
    .await;
    let mut sub = handle.subscribe();
    handle.submit("block").unwrap();
    let _ = wait_turn_id(&mut sub).await;
    handle.submit("!echo queued").unwrap();
    let _ = tx.send(());
    wait_settled(&mut sub).await;
    wait_settled(&mut sub).await;

    let texts = user_texts(&history(&handle));
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert_eq!(texts[0], "block");
    let parsed = LocalShell::try_parse(&texts[1]).unwrap();
    assert_eq!(parsed.command, "echo queued");
    handle.shutdown().await;
}

/// A response that reports its own token usage, so a test can tell whose
/// usage reached published state.
fn text_response_using(text: &str, usage: Usage) -> Response {
    Response {
        usage: Some(usage),
        ..text_response(text)
    }
}

const CHILD_USAGE: Usage = Usage {
    input_tokens: 900,
    output_tokens: 100,
    cache_read_tokens: 0,
    reasoning_tokens: 0,
};

#[tokio::test]
async fn task_runs_a_subagent_and_returns_its_report() {
    let tmp = tempdir::TempDir::new("app-runtime-subagent").unwrap();
    std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();
    let mock = MockProvider::new(vec![
        tool_response(
            "c1",
            "task",
            serde_json::json!({
                "description": "read note.txt",
                "prompt": "read note.txt and report what it says",
                "role": "explore",
            }),
        ),
        text_response_using("the file says hello", CHILD_USAGE),
        text_response("done"),
    ]);
    let app = AppBuilder::new(tmp.path());
    let agents = app.build_agent_with_provider(Box::new(mock)).await.unwrap();
    let main_id = agents.main.id();
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );
    let mut rx = handle.subscribe();

    assert_eq!(handle.prompt("delegate it").await.unwrap(), "done");

    let subagents = handle.subagents();
    assert_eq!(subagents.len(), 1, "{subagents:?}");
    assert_eq!(subagents[0].name, "explore#1");
    assert_eq!(subagents[0].role, "explore");
    assert_eq!(subagents[0].label, "read note.txt");
    assert_eq!(subagents[0].steps, 1, "the subagent ran one step");
    assert!(matches!(subagents[0].status, NodeStatus::Completed));

    let report_reached_the_model = history(&handle).iter().any(|message| {
        message.content.iter().any(|block| match block {
            ContentBlock::ToolResult { content, .. } => content
                .iter()
                .any(|block| matches!(block, ContentBlock::Text { text } if text.contains("the file says hello"))),
            _ => false,
        })
    });
    assert!(
        report_reached_the_model,
        "the subagent's report is the tool result"
    );

    let mut child_events = 0;
    let mut child_ids = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let AppEventKind::Agent(env) = &ev.kind
            && env.agent_id != main_id
        {
            child_events += 1;
            if !child_ids.contains(&env.agent_id) {
                child_ids.push(env.agent_id);
            }
        }
    }
    assert_eq!(child_ids.len(), 1, "one subagent reported");
    assert!(child_events > 0, "its events reached subscribers");
    handle.shutdown().await;
}

/// With subagents disabled the delegation tools are not mounted at all, so a
/// call to one fails the way any unknown tool does.
#[tokio::test]
async fn subagents_can_be_turned_off() {
    let tmp = tempdir::TempDir::new("app-runtime-subagent-off").unwrap();
    let mock = MockProvider::new(vec![
        tool_response(
            "c1",
            "task",
            serde_json::json!({ "description": "x", "prompt": "y" }),
        ),
        text_response("no subagent for me"),
    ]);
    let app = AppBuilder::new(tmp.path())
        .with_config(AppConfig {
            subagents: crate::core::config::SubagentConfig {
                enabled: false,
                ..Default::default()
            },
            ..AppConfig::default()
        })
        .await;
    let agents = app.build_agent_with_provider(Box::new(mock)).await.unwrap();
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        app.config().clone(),
        None,
    );

    assert_eq!(
        handle.prompt("delegate it").await.unwrap(),
        "no subagent for me"
    );
    assert!(handle.subagents().is_empty());
    let refused = history(&handle).iter().any(|message| {
        message.content.iter().any(|block| match block {
            ContentBlock::ToolResult { content, .. } => content
                .iter()
                .any(|block| matches!(block, ContentBlock::Text { text } if text.contains("unknown tool: task"))),
            _ => false,
        })
    });
    assert!(refused, "the unmounted tool is reported as unknown");
    handle.shutdown().await;
}

/// A response carrying several tool calls, which the model emits whenever it
/// wants work done in parallel.
fn parallel_tool_response(calls: &[(&str, &str, serde_json::Value)]) -> Response {
    Response {
        content: calls
            .iter()
            .map(|(id, name, input)| ContentBlock::ToolUse {
                id: (*id).into(),
                name: (*name).into(),
                input: input.clone(),
                raw_arguments: None,
            })
            .collect(),
        ..tool_response(calls[0].0, calls[0].1, calls[0].2.clone())
    }
}

/// Answers subagents only once both of them have asked: a driver that runs
/// its calls one after another can therefore never finish the turn.
struct RendezvousProvider {
    responses: std::sync::Mutex<std::collections::VecDeque<Response>>,
    arrived: tokio::sync::Barrier,
}

impl RendezvousProvider {
    fn new(responses: Vec<Response>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into()),
            arrived: tokio::sync::Barrier::new(2),
        }
    }
}

#[async_trait]
impl Provider for RendezvousProvider {
    async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
        if is_subagent(req) {
            self.arrived.wait().await;
            return Ok(text_response("the subagent's report"));
        }
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
        ProviderName::Custom("rendezvous".into())
    }
}

/// Two subagents asked for in one response have to run at the same time: each
/// waits for the other to arrive before answering.
#[tokio::test]
async fn tool_calls_in_one_step_run_concurrently() {
    let tmp = tempdir::TempDir::new("app-runtime-parallel-tasks").unwrap();
    let delegate = |id: &'static str, label: &str| {
        (
            id as &'static str,
            "task",
            serde_json::json!({
                "description": label,
                "prompt": format!("look into {label}"),
                "role": "explore",
            }),
        )
    };
    let mock = RendezvousProvider::new(vec![
        parallel_tool_response(&[delegate("c1", "one"), delegate("c2", "two")]),
        text_response("both done"),
    ]);
    let app = AppBuilder::new(tmp.path());
    let agents = app.build_agent_with_provider(Box::new(mock)).await.unwrap();
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );

    let out = tokio::time::timeout(Duration::from_secs(5), handle.prompt("delegate both"))
        .await
        .expect("both subagents must be able to run at once")
        .unwrap();
    assert_eq!(out, "both done");
    assert_eq!(handle.subagents().len(), 2);
    handle.shutdown().await;
}

/// A subagent's usage is its own: the driver's context readout must not move
/// when a subagent finishes.
#[tokio::test]
async fn a_subagent_usage_does_not_become_the_drivers_context() {
    let tmp = tempdir::TempDir::new("app-runtime-subagent-usage").unwrap();
    std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();
    let mock = MockProvider::new(vec![
        tool_response(
            "c1",
            "task",
            serde_json::json!({
                "description": "read note.txt",
                "prompt": "read note.txt",
                "role": "explore",
            }),
        ),
        text_response_using("report", CHILD_USAGE),
        text_response("done"),
    ]);
    let app = AppBuilder::new(tmp.path());
    let agents = app.build_agent_with_provider(Box::new(mock)).await.unwrap();
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );

    assert_eq!(handle.prompt("delegate it").await.unwrap(), "done");

    let state = handle.state();
    assert_eq!(
        state.last_turn_usage.input_tokens, 10,
        "published usage is the driver's last response"
    );
    assert_eq!(
        state.subagents[0].usage, CHILD_USAGE,
        "the subagent keeps its own"
    );
    handle.shutdown().await;
}

/// Answers a subagent's request slowly and the driver's from a script, so a
/// test can cancel while a subagent is mid-flight.
struct SlowSubagentProvider {
    responses: std::sync::Mutex<std::collections::VecDeque<Response>>,
    delay: Duration,
}

impl SlowSubagentProvider {
    fn new(responses: Vec<Response>, delay: Duration) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into()),
            delay,
        }
    }
}

#[async_trait]
impl Provider for SlowSubagentProvider {
    async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
        if is_subagent(req) {
            tokio::time::sleep(self.delay).await;
            return Ok(text_response("the subagent's report"));
        }
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
        ProviderName::Custom("slow-subagent".into())
    }
}

/// A subagent runs with its own system prompt, which is what tells its
/// requests apart from the driver's.
fn is_subagent(req: &Request) -> bool {
    req.system
        .as_deref()
        .is_some_and(|system| system.contains("You are a subagent"))
}

/// A view command has to work while the turn that spawned the subagent is
/// still running: deferring `/agents` until the reply lands would mean you can
/// never watch a subagent work. Commands that need the driver still wait.
#[tokio::test]
async fn agents_applies_mid_turn_while_clear_waits() {
    let tmp = tempdir::TempDir::new("app-runtime-agents-mid-turn").unwrap();
    std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();
    let mock = SlowSubagentProvider::new(
        vec![tool_response(
            "c1",
            "task",
            serde_json::json!({
                "description": "read note.txt",
                "prompt": "read note.txt",
                "role": "explore",
            }),
        )],
        Duration::from_secs(30),
    );
    let app = AppBuilder::new(tmp.path());
    let agents = app.build_agent_with_provider(Box::new(mock)).await.unwrap();
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );
    let mut rx = handle.subscribe();
    handle.submit("delegate it").unwrap();

    wait_for_active_subagent(&handle).await;
    assert!(
        handle.state().phase.is_active(),
        "the turn is still running"
    );
    handle.submit("/agents").unwrap();
    handle.submit("/clear").unwrap();

    let mut listed = false;
    let mut cleared_queued = false;
    while let Some(ev) = rx.recv().await {
        match &ev.kind {
            AppEventKind::Notification { text } if text.contains("queued:") => {
                assert!(
                    text.contains("/clear"),
                    "only the command that needs the driver may wait: {text}"
                );
                cleared_queued = true;
            }
            AppEventKind::Notification { text } if text.contains("explore#1") => {
                listed = true;
            }
            _ => {}
        }
        if listed && cleared_queued {
            break;
        }
    }
    assert!(listed, "the listing arrived while the turn ran");
    assert!(cleared_queued, "the history-clearing command waited");
    handle.shutdown().await;
}

/// The text of the last thing the user said, which is what tells one turn of
/// a scripted conversation from the next.
fn user_prompt(req: &Request) -> String {
    req.messages
        .iter()
        .rev()
        .find(|message| message.role == Role::User)
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>()
        })
        .unwrap_or_default()
}

fn already_ran_a_tool(req: &Request) -> bool {
    req.messages.iter().any(|message| {
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
    })
}

/// A first turn that delegates to a subagent the test releases by hand, and a
/// second that never finishes — enough to hold a message in the queue for as
/// long as the assertion needs.
struct UnsentProvider {
    release: Arc<tokio::sync::Notify>,
}

impl UnsentProvider {
    fn new() -> (Self, Arc<tokio::sync::Notify>) {
        let release = Arc::new(tokio::sync::Notify::new());
        (
            Self {
                release: Arc::clone(&release),
            },
            release,
        )
    }
}

#[async_trait]
impl Provider for UnsentProvider {
    async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
        if is_subagent(req) {
            self.release.notified().await;
            return Ok(text_response("the subagent's report"));
        }
        if user_prompt(req).contains("two") {
            std::future::pending::<()>().await;
        }
        if already_ran_a_tool(req) {
            return Ok(text_response("the first turn is done"));
        }
        Ok(tool_response(
            "c1",
            "task",
            serde_json::json!({
                "description": "read note.txt",
                "prompt": "read note.txt",
                "role": "explore",
            }),
        ))
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
        ProviderName::Custom("unsent".into())
    }
}

/// A message handed over while a turn runs never gets its turn if the app
/// quits first, so the runtime counts it and hands the number back.
#[tokio::test]
async fn shutdown_reports_prompts_that_never_ran() {
    let tmp = tempdir::TempDir::new("app-runtime-unsent").unwrap();
    std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();
    let (mock, release) = UnsentProvider::new();
    let app = AppBuilder::new(tmp.path());
    let agents = app.build_agent_with_provider(Box::new(mock)).await.unwrap();
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );
    let main = handle.agent_id();
    let mut rx = handle.subscribe();
    handle.submit("one").unwrap();

    // Both arrive while the first turn is still waiting on its subagent, so
    // neither can start a turn yet.
    wait_for_active_subagent(&handle).await;
    handle.submit("two").unwrap();
    handle.submit("three").unwrap();

    // Let the subagent go: the runtime takes the first of the two messages
    // and starts a turn for it, which stays open. The second waits behind it.
    release.notify_one();
    let mut started = 0;
    while let Some(ev) = rx.recv().await {
        started += usize::from(turn_started(&ev, main));
        if started == 2 {
            break;
        }
    }

    assert_eq!(
        handle.shutdown().await,
        1,
        "only the message still waiting went unsent"
    );
}

/// Cancelling a turn cancels the subagent it spawned, rather than leaving it
/// running behind an abandoned tool call.
#[tokio::test]
async fn cancelling_a_turn_cancels_its_subagent() {
    let tmp = tempdir::TempDir::new("app-runtime-subagent-cancel").unwrap();
    std::fs::write(tmp.path().join("note.txt"), "hello").unwrap();
    let mock = SlowSubagentProvider::new(
        vec![tool_response(
            "c1",
            "task",
            serde_json::json!({
                "description": "read note.txt",
                "prompt": "read note.txt",
                "role": "explore",
            }),
        )],
        Duration::from_secs(30),
    );
    let app = AppBuilder::new(tmp.path());
    let agents = app.build_agent_with_provider(Box::new(mock)).await.unwrap();
    let handle = spawn_runtime(
        AppId::next(),
        agents,
        None,
        tmp.path().to_path_buf(),
        AppConfig::default(),
        None,
    );
    handle.submit("delegate it").unwrap();

    wait_for_active_subagent(&handle).await;
    let turn_id = handle.state().phase.turn_id().expect("turn is running");
    handle.cancel(turn_id);
    wait_state(&handle, |state| state.phase.is_idle()).await;

    let subagents = handle.subagents();
    assert!(
        matches!(subagents[0].status, NodeStatus::Cancelled),
        "{subagents:?}"
    );
    handle.shutdown().await;
}

async fn memory_store(workspace: &Path, user: &Path) -> Arc<oven_mem::MemoryStore> {
    Arc::new(
        oven_mem::MemoryStore::load(oven_mem::MemoryRoots {
            workspace: workspace.to_path_buf(),
            user: Some(user.to_path_buf()),
        })
        .await,
    )
}

fn write_memory_file(dir: &Path, id: &str, kind: &str, description: &str, body: &str, secs: u64) {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{id}.md"));
    std::fs::write(
        &path,
        format!("---\nkind: {kind}\ndescription: {description}\nsource: session-1\n---\n\n{body}"),
    )
    .unwrap();
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs))
        .unwrap();
}

async fn spawn_memory_app(tmp: &tempdir::TempDir, store: Arc<oven_mem::MemoryStore>) -> App {
    let mut app = AppBuilder::new(tmp.path());
    app.set_memory(store);
    spawn_app(&app, Box::new(MockProvider::new(vec![]))).await
}

#[tokio::test]
async fn slash_memory_lists_shows_and_removes() {
    let tmp = tempdir::TempDir::new("app-runtime-memory").unwrap();
    let workspace = tmp.path().join("workspace");
    let user = tmp.path().join("user");
    let empty = spawn_memory_app(&tmp, memory_store(&workspace, &user).await).await;
    assert_eq!(empty.prompt("/memory").await.unwrap(), NO_MEMORIES);
    empty.shutdown().await;

    write_memory_file(&workspace, "older", "fact", "old fact", "old body\n", 10);
    write_memory_file(&user, "newer", "preference", "new pref", "new body\n", 20);
    let handle = spawn_memory_app(&tmp, memory_store(&workspace, &user).await).await;
    assert_eq!(
        handle.prompt("/memory").await.unwrap(),
        "user/newer (preference) new pref\nworkspace/older (fact) old fact"
    );
    assert_eq!(
        handle.prompt("/memory show workspace/older").await.unwrap(),
        format!(
            "{KIND_LABEL}: fact\n{DESCRIPTION_LABEL}: old fact\n{SOURCE_LABEL}: session-1\n\nold body\n",
        )
    );
    assert_eq!(
        handle.prompt("/memory rm user/newer").await.unwrap(),
        format!("{REMOVED_MEMORY}: user/newer")
    );
    assert!(!user.join("newer.md").exists());
    assert_eq!(
        handle.prompt("/memory").await.unwrap(),
        "workspace/older (fact) old fact"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn slash_memory_ambiguous_bare_id_and_unknown_rm_are_replies() {
    let tmp = tempdir::TempDir::new("app-runtime-memory-ref").unwrap();
    let workspace = tmp.path().join("workspace");
    let user = tmp.path().join("user");
    write_memory_file(&workspace, "shared", "fact", "from workspace", "ws\n", 1);
    write_memory_file(&user, "shared", "fact", "from user", "user\n", 2);
    let handle = spawn_memory_app(&tmp, memory_store(&workspace, &user).await).await;
    assert_eq!(
        handle.prompt("/memory show shared").await.unwrap(),
        format!(
            "{AMBIGUOUS_MEMORY}: {}/shared or {}/shared",
            oven_mem::MemoryScope::Workspace,
            oven_mem::MemoryScope::User
        )
    );
    let missing = handle.prompt("/memory rm missing").await.unwrap();
    assert_eq!(missing, format!("{NOT_FOUND}: missing"));
    assert!(user.join("shared.md").is_file());
    handle.shutdown().await;
}

#[tokio::test]
async fn slash_memory_disabled_replies() {
    let tmp = tempdir::TempDir::new("app-runtime-memory-off").unwrap();
    let app = AppBuilder::new(tmp.path())
        .with_config(AppConfig {
            memory: crate::config::MemoryConfig { enabled: false },
            ..AppConfig::default()
        })
        .await;
    let handle = spawn_app(&app, Box::new(MockProvider::new(vec![]))).await;
    assert_eq!(handle.prompt("/memory").await.unwrap(), MEMORY_DISABLED);
    handle.shutdown().await;
}

#[tokio::test]
async fn memory_source_follows_the_session_after_clear() {
    let tmp = tempdir::TempDir::new("app-runtime-memory-source").unwrap();
    let sessions = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let workspace = tmp.path().join(".oven").join("memory");
    let mut app = AppBuilder::new(tmp.path());
    app.set_memory(memory_store(&workspace, &tmp.path().join("unused-user")).await);
    let write = |id: &str| {
        tool_response(
            "c1",
            crate::capabilities::memory::MemoryWriteTool::NAME,
            serde_json::json!({
                "scope": "workspace",
                "id": id,
                "kind": "fact",
                "description": id,
                "body": "body\n",
            }),
        )
    };
    let session = Session::open(&sessions, "session-one").await.unwrap();
    let handle = spawn_app_session(
        &app,
        Box::new(MockProvider::new(vec![
            write("first-fact"),
            text_response("saved"),
            write("second-fact"),
            text_response("saved again"),
        ])),
        session,
    )
    .await;
    assert_eq!(handle.prompt("remember one").await.unwrap(), "saved");
    handle.prompt("/clear").await.unwrap();
    assert_eq!(handle.prompt("remember two").await.unwrap(), "saved again");
    let new_id = handle
        .session_id()
        .expect("the second write's session has content");
    assert_ne!(new_id, "session-one");

    let store = memory_store(&workspace, &tmp.path().join("unused-user")).await;
    let first = store
        .read(
            oven_mem::MemoryScope::Workspace,
            &oven_mem::MemoryId::new("first-fact").unwrap(),
        )
        .await
        .unwrap();
    let second = store
        .read(
            oven_mem::MemoryScope::Workspace,
            &oven_mem::MemoryId::new("second-fact").unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.source.as_deref(), Some("session-one"));
    assert_eq!(second.source.as_deref(), Some(new_id.as_str()));
    handle.shutdown().await;
}

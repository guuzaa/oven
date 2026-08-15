use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oven_agent::{Agent, AgentEvent, CancellationToken, Record};
use oven_llm::{ContentBlock, Message, ModelInfo, Provider, ProviderName, Role, Usage};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::session::{Session, canonical_root, record_recent};
use crate::{App, AppError};

/// Id for one long-lived oven-app instance inside a TUI process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AppId(pub u64);

/// Event fan-out for one runtime. Each subscriber gets its own lossless
/// unbounded channel, so a slow UI never silently drops streaming chunks
/// the way a broadcast receiver would when it lags.
type Subscribers = Arc<Mutex<Vec<mpsc::UnboundedSender<AppEvent>>>>;

/// Commands sent from TUI / CLI into an app task.
#[derive(Debug, Clone)]
pub enum AppCmd {
    UserInput(String),
    Cancel,
    /// Drop the last user turn from the conversation (in-memory history and
    /// the persisted session file) so its message can be edited and resent.
    Rewind,
    Shutdown,
}

/// Events emitted by an app task (agent events plus app lifecycle).
#[derive(Debug, Clone)]
pub enum AppEvent {
    Agent {
        app_id: AppId,
        event: AgentEvent,
    },
    ModelsUpdated {
        app_id: AppId,
        models: Vec<(String, String)>,
    },
    Idle {
        app_id: AppId,
    },
    Error {
        app_id: AppId,
        message: String,
    },
    /// One exchange was rewound by the TUI: `text` is the removed user
    /// message (joined text blocks), `messages` is the truncated history,
    /// and `usage` is the cumulative token usage after the rollback.
    Rewound {
        app_id: AppId,
        text: Option<String>,
        messages: Vec<Message>,
        usage: Usage,
    },
}

/// Snapshot of the current persisted session, shared between the runtime
/// task and its `AppHandle`. The runtime task owns the source of truth and
/// updates this whenever the session switches or its content changes.
struct SharedSession {
    session: Session,
    has_content: bool,
}

/// Persisted session tracking for one runtime. `/clear` replaces the shared
/// current session with a fresh uuid v7 session so the old file is left
/// untouched.
struct SessionStore {
    dir: PathBuf,
    root: String,
    shared: Arc<Mutex<SharedSession>>,
}

impl SessionStore {
    /// The current session (id + path), cloned out of the shared snapshot.
    fn current(&self) -> Session {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .session
            .clone()
    }

    /// Replace the current session; a fresh session starts without content.
    fn set_current(&self, session: Session) {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.session = session;
        shared.has_content = false;
    }

    /// Update whether the current session holds conversation content.
    fn mark_content(&self, has_content: bool) {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_content = has_content;
    }
}

/// Handle to a running oven-app actor.
pub struct AppHandle {
    id: AppId,
    cmd_tx: mpsc::UnboundedSender<AppCmd>,
    subscribers: Subscribers,
    join: JoinHandle<()>,
    slash_commands: Vec<(String, String)>,
    model: String,
    root: PathBuf,
    /// Cumulative token usage at spawn time: the restored usage of a resumed
    /// session, or zero for a fresh one. Startup snapshot, updated afterwards
    /// via `AgentEvent::Done` / `AppEvent::Rewound`.
    total_usage: Usage,
    /// Conversation history snapshot taken when the runtime was spawned.
    history: Vec<Message>,
    /// Current persisted session snapshot, `None` when this runtime has no
    /// session persistence. Updated live when `/clear` switches sessions.
    session: Option<Arc<Mutex<SharedSession>>>,
}

impl AppHandle {
    pub fn id(&self) -> AppId {
        self.id
    }

    pub fn send(&self, cmd: AppCmd) -> Result<(), AppError> {
        self.cmd_tx.send(cmd).map_err(|_| AppError::ChannelClosed)
    }

    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<AppEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    /// (name, description) pairs for the registered slash commands.
    pub fn slash_commands(&self) -> &[(String, String)] {
        &self.slash_commands
    }

    /// Model name in effect for this runtime.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Workspace root the runtime was started with.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Messages loaded at spawn time (empty for a fresh session). This is a
    /// startup snapshot, not a live view of the in-flight conversation.
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// Token usage loaded at spawn time (zero for a fresh session). This is a
    /// startup snapshot; later changes arrive as events.
    pub fn total_usage(&self) -> Usage {
        self.total_usage
    }

    /// Current persisted session id for this runtime. Returns `None` when the
    /// runtime was spawned without session persistence, or when the current
    /// session has no conversation content yet (nothing was ever written to
    /// it). Tracks `/clear` switches.
    pub fn session_id(&self) -> Option<String> {
        let shared = self.session.as_ref()?;
        let shared = shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.has_content.then(|| shared.session.id().to_string())
    }

    /// Send one user turn and wait until the app returns to [`AppEvent::Idle`].
    /// Returns the final assistant text from [`AgentEvent::Done`] when present.
    pub async fn prompt(&self, input: impl Into<String>) -> Result<String, AppError> {
        let mut rx = self.subscribe();
        self.send(AppCmd::UserInput(input.into()))?;

        let mut text = String::new();
        loop {
            match rx.recv().await {
                Some(AppEvent::Agent {
                    event: AgentEvent::Done { text: t, .. },
                    ..
                }) => text = t,
                Some(AppEvent::Idle { .. }) => return Ok(text),
                Some(AppEvent::Error { message, .. }) => {
                    return Err(AppError::Runtime(message));
                }
                Some(_) => {}
                None => return Err(AppError::ChannelClosed),
            }
        }
    }

    /// Request shutdown and wait for the app task to finish.
    pub async fn shutdown(self) {
        let _ = self.cmd_tx.send(AppCmd::Shutdown);
        let _ = self.join.await;
    }
}

impl App {
    /// Spawn a long-lived app task with no session persistence.
    pub async fn spawn(&self) -> Result<AppHandle, AppError> {
        let agent = self.build_agent().await?;
        Ok(spawn_runtime(
            AppId::next(),
            agent,
            None,
            self.config.provider.effective_model(),
            self.root.clone(),
            self.model_list_timeout(),
        ))
    }

    /// Spawn with a persisted session under the platform data dir. `Some(id)`
    /// resumes that session when its file exists; otherwise (or for `None`) a
    /// new session is started with an auto-generated uuid v7 id that the
    /// caller never has to provide.
    pub async fn spawn_session(&self, session_id: Option<&str>) -> Result<AppHandle, AppError> {
        let Some(dir) = crate::session::default_sessions_dir() else {
            return self.spawn().await;
        };
        self.spawn_session_in(&dir, session_id).await
    }

    /// Same as [`App::spawn_session`] with an explicit sessions directory.
    pub async fn spawn_session_in(
        &self,
        sessions_dir: &Path,
        session_id: Option<&str>,
    ) -> Result<AppHandle, AppError> {
        let session = resolve_session(sessions_dir, session_id)?;
        let prior = session.load_records()?;
        let mut agent = self.build_agent().await?;
        let records: Vec<_> = prior
            .into_iter()
            .filter(
                |r| !matches!(r, Record::Message { message, .. } if message.role == Role::System),
            )
            .collect();
        agent.restore_history(records);
        agent.ensure_session_meta(canonical_root(&self.root));
        Ok(spawn_runtime(
            AppId::next(),
            agent,
            Some(session),
            self.config.provider.effective_model(),
            self.root.clone(),
            self.model_list_timeout(),
        ))
    }

    /// Test/custom wiring variant of [`App::spawn_session_in`] with an
    /// explicit provider.
    pub async fn spawn_session_with_provider_in(
        &self,
        sessions_dir: &Path,
        provider: Box<dyn Provider>,
        session_id: Option<&str>,
    ) -> Result<AppHandle, AppError> {
        let session = resolve_session(sessions_dir, session_id)?;
        let prior = session.load_records()?;
        let mut agent = self.build_agent_with_provider(provider).await?;
        let records: Vec<_> = prior
            .into_iter()
            .filter(
                |r| !matches!(r, Record::Message { message, .. } if message.role == Role::System),
            )
            .collect();
        agent.restore_history(records);
        agent.ensure_session_meta(canonical_root(&self.root));
        Ok(spawn_runtime(
            AppId::next(),
            agent,
            Some(session),
            self.config.provider.effective_model(),
            self.root.clone(),
            self.model_list_timeout(),
        ))
    }

    /// Spawn with an explicit provider (tests / custom wiring).
    pub async fn spawn_with_provider(
        &self,
        provider: Box<dyn Provider>,
    ) -> Result<AppHandle, AppError> {
        let agent = self.build_agent_with_provider(provider).await?;
        Ok(spawn_runtime(
            AppId::next(),
            agent,
            None,
            self.config.provider.effective_model(),
            self.root.clone(),
            self.model_list_timeout(),
        ))
    }

    /// Spawn with provider + session store (tests).
    pub async fn spawn_with_provider_session(
        &self,
        provider: Box<dyn Provider>,
        session: Session,
    ) -> Result<AppHandle, AppError> {
        let prior = session.load_records()?;
        let mut agent = self.build_agent_with_provider(provider).await?;
        let records: Vec<_> = prior
            .into_iter()
            .filter(
                |r| !matches!(r, Record::Message { message, .. } if message.role == Role::System),
            )
            .collect();
        agent.restore_history(records);
        agent.ensure_session_meta(canonical_root(&self.root));
        Ok(spawn_runtime(
            AppId::next(),
            agent,
            Some(session),
            self.config.provider.effective_model(),
            self.root.clone(),
            self.model_list_timeout(),
        ))
    }
}

impl AppId {
    fn next() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

fn resolve_session(sessions_dir: &Path, session_id: Option<&str>) -> Result<Session, AppError> {
    let session = match session_id {
        Some(id) => {
            let candidate = Session::open(sessions_dir, id)?;
            if candidate.path().exists() {
                candidate
            } else {
                let uuid = uuid::Uuid::now_v7().to_string();
                Session::open(sessions_dir, &uuid)?
            }
        }
        None => {
            let uuid = uuid::Uuid::now_v7().to_string();
            Session::open(sessions_dir, &uuid)?
        }
    };
    Ok(session)
}

fn spawn_runtime(
    app_id: AppId,
    agent: Agent,
    session: Option<Session>,
    model: String,
    root: PathBuf,
    model_list_timeout: Duration,
) -> AppHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
    let subscribers_task = Arc::clone(&subscribers);
    let slash_commands = agent.slash_commands();
    let history: Vec<Message> = agent.history().cloned().collect();
    let total_usage = *agent.total_usage();
    let (session_store, shared_session) = match session {
        Some(s) => {
            let shared = Arc::new(Mutex::new(SharedSession {
                session: s.clone(),
                has_content: agent.history().len() != 0,
            }));
            let dir = s.path().parent().map(Path::to_path_buf).unwrap_or_default();
            (
                Some(SessionStore {
                    dir,
                    root: canonical_root(&root),
                    shared: shared.clone(),
                }),
                Some(shared),
            )
        }
        None => (None, None),
    };
    let task_model = model.clone();
    let join = tokio::spawn(async move {
        runtime_loop(
            app_id,
            agent,
            session_store,
            cmd_rx,
            subscribers_task,
            task_model,
            model_list_timeout,
        )
        .await;
    });
    AppHandle {
        id: app_id,
        cmd_tx,
        subscribers,
        join,
        slash_commands,
        model,
        root,
        total_usage,
        history,
        session: shared_session,
    }
}

fn emit(subs: &Subscribers, event: AppEvent) {
    let mut subs = subs.lock().unwrap_or_else(|e| e.into_inner());
    subs.retain(|tx| tx.send(event.clone()).is_ok());
}

fn switch_session(store: &mut Option<SessionStore>, subs: &Subscribers, app_id: AppId) {
    if let Some(store) = store {
        let id = uuid::Uuid::now_v7().to_string();
        match Session::open(&store.dir, &id) {
            Ok(next) => store.set_current(next),
            Err(e) => {
                emit(
                    subs,
                    AppEvent::Error {
                        app_id,
                        message: e.to_string(),
                    },
                );
            }
        }
    }
}

/// Best-effort model list for the `/model` popup: static presets merged with
/// the provider's dynamic `/models` listing and the current model. A failed or
/// timed-out dynamic fetch silently falls back to the static data.
async fn refresh_model_choices(
    provider: &dyn Provider,
    current_model: &str,
    timeout: Duration,
) -> Vec<(String, String)> {
    let known = provider.known_models();
    let dynamic = match tokio::time::timeout(timeout, provider.list_models()).await {
        Ok(Ok(list)) => list,
        _ => Vec::new(),
    };
    merge_model_choices(known, dynamic, current_model, &provider.provider_name())
}

fn merge_model_choices(
    known: Vec<ModelInfo>,
    dynamic: Vec<ModelInfo>,
    current_model: &str,
    current_provider: &ProviderName,
) -> Vec<(String, String)> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let choices = std::iter::once((current_model.to_string(), current_provider.clone()))
        .chain(known.into_iter().map(|m| (m.id, m.provider)))
        .chain(dynamic.into_iter().map(|m| (m.id, m.provider)));
    for (id, provider) in choices {
        if seen.insert(id.clone()) {
            out.push((id, provider_label(&provider)));
        }
    }
    out
}

fn provider_label(name: &ProviderName) -> String {
    match name {
        ProviderName::Custom(name) => format!("Custom({name})"),
        other => format!("{other:?}"),
    }
}

/// Text of a user message: its text blocks joined with newlines. Empty when
/// the message carries no text (e.g. embedded tool results only).
fn user_message_text(m: &Message) -> String {
    m.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn runtime_loop(
    app_id: AppId,
    mut agent: Agent,
    mut session_store: Option<SessionStore>,
    mut cmd_rx: mpsc::UnboundedReceiver<AppCmd>,
    subscribers: Subscribers,
    model: String,
    model_list_timeout: Duration,
) {
    // Leading in-memory messages already written to the store. On `/clear`
    // the in-memory history is replaced, so this resets to 0 while the store
    // itself is kept untouched; later turns append after the old content.
    // A brand-new session holds a meta record in memory but nothing on disk
    // yet, so its prefix starts at 0 and the first append writes the meta.
    let mut persisted_prefix = match &session_store {
        Some(store) if store.current().path().exists() => agent.history_records().len(),
        _ => 0,
    };
    let mut persisted_rev = agent.history_revision();

    // Publish the initial model list for the `/model` popup completion.
    let models = refresh_model_choices(agent.provider(), &model, model_list_timeout).await;
    emit(&subscribers, AppEvent::ModelsUpdated { app_id, models });

    // Commands received while a turn is in flight are buffered here and run
    // in order once the current turn finishes, so UserInput is never dropped.
    let mut pending_cmds: VecDeque<AppCmd> = VecDeque::new();

    loop {
        let cmd = match pending_cmds.pop_front() {
            Some(cmd) => cmd,
            None => match cmd_rx.recv().await {
                Some(cmd) => cmd,
                None => break,
            },
        };
        match cmd {
            AppCmd::Shutdown => break,
            AppCmd::Cancel => {
                // no in-flight turn
            }
            AppCmd::Rewind => {
                let removed = agent.rewind_last_turn();
                let text = removed.map(|m| user_message_text(&m));
                let store_ok = match &session_store {
                    Some(store) => {
                        let records = agent.history_records();
                        match store.current().overwrite(&records) {
                            Ok(()) => {
                                store.mark_content(agent.history().len() != 0);
                                true
                            }
                            Err(e) => {
                                emit(
                                    &subscribers,
                                    AppEvent::Error {
                                        app_id,
                                        message: e.to_string(),
                                    },
                                );
                                false
                            }
                        }
                    }
                    None => true,
                };
                if store_ok {
                    persisted_prefix = agent.history_records().len();
                }
                persisted_rev = agent.history_revision();
                emit(
                    &subscribers,
                    AppEvent::Rewound {
                        app_id,
                        text,
                        messages: agent.history().cloned().collect(),
                        usage: *agent.total_usage(),
                    },
                );
            }
            AppCmd::UserInput(input) => {
                let cancel = CancellationToken::new();
                let (agent_tx, mut agent_rx) = mpsc::unbounded_channel();

                let result = {
                    let turn = agent.run_with_emitter(input, Some(agent_tx), Some(&cancel));
                    tokio::pin!(turn);

                    loop {
                        tokio::select! {
                            biased;
                            cmd = cmd_rx.recv() => {
                                match cmd {
                                    None | Some(AppCmd::Shutdown) => {
                                        cancel.cancel();
                                        let _ = turn.await;
                                        return;
                                    }
                                    Some(AppCmd::Cancel) => cancel.cancel(),
                                    Some(AppCmd::UserInput(input)) => {
                                        pending_cmds.push_back(AppCmd::UserInput(input));
                                    }
                                    Some(AppCmd::Rewind) => {
                                        pending_cmds.push_back(AppCmd::Rewind);
                                    }
                                }
                            }
                            ev = agent_rx.recv() => {
                                match ev {
                                    Some(event) => {
                                        if matches!(&event, AgentEvent::HistoryCleared { .. }) {
                                            switch_session(
                                                &mut session_store,
                                                &subscribers,
                                                app_id,
                                            );
                                        }
                                        emit(&subscribers, AppEvent::Agent { app_id, event });
                                    }
                                    None => break turn.await,
                                }
                            }
                            res = &mut turn => break res,
                        }
                    }
                };

                // A `/clear` during the turn switched to a fresh session whose
                // meta is still unset; stamp it with this workspace root now
                // that the turn (and its borrow of `agent`) has completed.
                if let Some(store) = &session_store {
                    agent.ensure_session_meta(store.root.clone());
                }

                while let Ok(event) = agent_rx.try_recv() {
                    if matches!(&event, AgentEvent::HistoryCleared { .. }) {
                        switch_session(&mut session_store, &subscribers, app_id);
                        if let Some(store) = &session_store {
                            agent.ensure_session_meta(store.root.clone());
                        }
                    }
                    emit(&subscribers, AppEvent::Agent { app_id, event });
                }

                match result {
                    Ok(_) => {
                        if let Some(store) = &session_store {
                            let rev = agent.history_revision();
                            let after = agent.history_records();
                            if rev != persisted_rev {
                                // History was replaced in memory (`/clear`):
                                // keep the store untouched and treat the new
                                // in-memory chat as unpersisted.
                                persisted_prefix = 0;
                                persisted_rev = rev;
                            } else if after.len() > persisted_prefix {
                                if let Err(e) =
                                    store.current().append_records(&after[persisted_prefix..])
                                {
                                    emit(
                                        &subscribers,
                                        AppEvent::Error {
                                            app_id,
                                            message: e.to_string(),
                                        },
                                    );
                                } else {
                                    store.mark_content(true);
                                    persisted_prefix = after.len();
                                    if let Err(e) = record_recent(
                                        &store.dir,
                                        Path::new(&store.root),
                                        store.current().id(),
                                    ) {
                                        emit(
                                            &subscribers,
                                            AppEvent::Error {
                                                app_id,
                                                message: e.to_string(),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) if e.is_cancelled() => {}
                    Err(e) => {
                        emit(
                            &subscribers,
                            AppEvent::Error {
                                app_id,
                                message: e.to_string(),
                            },
                        );
                    }
                }

                emit(&subscribers, AppEvent::Idle { app_id });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use oven_llm::{
        ContentBlock, ModelId, ModelInfo, Provider, ProviderError, ProviderName, Request, Response,
        Role, StopReason, StreamEvent, Usage,
    };
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

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
        let handle = app.spawn_with_provider(Box::new(mock)).await.unwrap();

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
        let handle = app.spawn_with_provider(Box::new(mock)).await.unwrap();

        let names: Vec<&str> = handle
            .slash_commands()
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(names, ["clear", "exit", "model"]);
        assert!(handle.slash_commands().iter().all(|(_, d)| !d.is_empty()));

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn model_slash_switch_uses_request_model() {
        let seen = recorder();
        let agent = Agent::new(Box::new(RecordingProvider::new(seen.clone())), Vec::new())
            .with_model("gpt-4o");
        let handle = spawn_runtime(
            AppId::next(),
            agent,
            None,
            "gpt-4o".into(),
            PathBuf::from("/tmp"),
            Duration::from_secs(1),
        );

        let out = handle.prompt("/model gpt-4o-turbo low").await.unwrap();
        assert_eq!(out, "model switched to gpt-4o-turbo (effort: low)");
        // The switch only changes the model carried by subsequent requests;
        // the provider object is never rebuilt or replaced.
        assert_eq!(handle.prompt("hello").await.unwrap(), "echo:gpt-4o-turbo");
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn slash_exit_emits_exit_event() {
        let tmp = tempdir::TempDir::new("app-runtime-exit").unwrap();
        let app = App::new(tmp.path());
        let mock = MockProvider::new(vec![]);
        let handle = app.spawn_with_provider(Box::new(mock)).await.unwrap();

        let mut rx = handle.subscribe();
        handle.send(AppCmd::UserInput("/exit".into())).unwrap();

        let mut saw_exit = false;
        while let Some(ev) = rx.recv().await {
            if let AppEvent::Agent {
                event: AgentEvent::Exit { .. },
                ..
            } = ev
            {
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
    async fn session_persists_across_spawns() {
        let tmp = tempdir::TempDir::new("app-runtime-sess").unwrap();
        let app = App::new(tmp.path());
        let dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&dir).unwrap();

        let mock1 = MockProvider::new(vec![text_response("one")]);
        let session = Session::open(&dir, "s1").unwrap();
        let handle = app
            .spawn_with_provider_session(Box::new(mock1), session)
            .await
            .unwrap();
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
        let handle = app
            .spawn_with_provider_session(Box::new(mock2), session)
            .await
            .unwrap();
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
        let handle = app
            .spawn_with_provider_session(Box::new(mock1), session)
            .await
            .unwrap();
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
        let handle = app
            .spawn_with_provider_session(Box::new(mock2), session)
            .await
            .unwrap();
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
        let handle = app
            .spawn_with_provider_session(Box::new(mock), session)
            .await
            .unwrap();
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
        let handle = app
            .spawn_session_with_provider_in(&dir, Box::new(mock), Some("missing"))
            .await
            .unwrap();
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
        let handle = app
            .spawn_session_with_provider_in(&dir, Box::new(mock), None)
            .await
            .unwrap();
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
        let handle = app
            .spawn_with_provider_session(Box::new(mock), session)
            .await
            .unwrap();
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
        let handle = app
            .spawn_session_with_provider_in(&dir, Box::new(mock), None)
            .await
            .unwrap();
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
        let handle = app
            .spawn_with_provider_session(Box::new(mock1), session)
            .await
            .unwrap();
        assert_eq!(handle.prompt("first").await.unwrap(), "one");
        handle.shutdown().await;

        let mock2 = MockProvider::new(vec![text_response("two")]);
        let handle = app
            .spawn_session_with_provider_in(&dir, Box::new(mock2), Some("s1"))
            .await
            .unwrap();
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
            ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError>
            {
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
        let handle = app.spawn_with_provider(Box::new(provider)).await.unwrap();
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
            ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError>
            {
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
        let handle = app.spawn_with_provider(Box::new(provider)).await.unwrap();
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
        let handle = app
            .spawn_session_with_provider_in(&dir, Box::new(mock), None)
            .await
            .unwrap();
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
        let handle = app
            .spawn_with_provider_session(Box::new(mock), session)
            .await
            .unwrap();
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
        let handle = app.spawn_with_provider(Box::new(mock)).await.unwrap();

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
        let handle = app.spawn_with_provider(Box::new(mock)).await.unwrap();

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
        let handle = app
            .spawn_with_provider_session(Box::new(mock), session)
            .await
            .unwrap();
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
        let handle = app
            .spawn_with_provider_session(Box::new(mock), session)
            .await
            .unwrap();
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
            ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError>
            {
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
        let handle = app.spawn_with_provider(Box::new(provider)).await.unwrap();
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
}

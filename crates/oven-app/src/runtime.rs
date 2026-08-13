use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use oven_agent::{Agent, AgentEvent, CancellationToken};
use oven_llm::{Message, Provider, Role};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::session::Session;
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
    Shutdown,
}

/// Events emitted by an app task (agent events plus app lifecycle).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    Agent { app_id: AppId, event: AgentEvent },
    Idle { app_id: AppId },
    Error { app_id: AppId, message: String },
}

/// Persisted session tracking for one runtime. `/clear` replaces `current`
/// with a fresh uuid v7 session so the old file is left untouched.
struct SessionStore {
    dir: PathBuf,
    current: Session,
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
    /// Conversation history snapshot taken when the runtime was spawned.
    history: Vec<Message>,
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
            self.effective_model(),
            self.root.clone(),
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
        let prior = session.load()?;
        let mut agent = self.build_agent().await?;
        for m in prior.into_iter().filter(|m| m.role != Role::System) {
            agent.push_history(m);
        }
        Ok(spawn_runtime(
            AppId::next(),
            agent,
            Some(session),
            self.effective_model(),
            self.root.clone(),
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
        let prior = session.load()?;
        let mut agent = self.build_agent_with_provider(provider).await?;
        for m in prior.into_iter().filter(|m| m.role != Role::System) {
            agent.push_history(m);
        }
        Ok(spawn_runtime(
            AppId::next(),
            agent,
            Some(session),
            self.effective_model(),
            self.root.clone(),
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
            self.effective_model(),
            self.root.clone(),
        ))
    }

    /// Spawn with provider + session store (tests).
    pub async fn spawn_with_provider_session(
        &self,
        provider: Box<dyn Provider>,
        session: Session,
    ) -> Result<AppHandle, AppError> {
        let prior = session.load()?;
        let mut agent = self.build_agent_with_provider(provider).await?;
        for m in prior.into_iter().filter(|m| m.role != Role::System) {
            agent.push_history(m);
        }
        Ok(spawn_runtime(
            AppId::next(),
            agent,
            Some(session),
            self.effective_model(),
            self.root.clone(),
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
) -> AppHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
    let subscribers_task = Arc::clone(&subscribers);
    let slash_commands = agent.slash_commands();
    let history = agent.history().to_vec();
    let session_store = session.map(|s| {
        let dir = s.path().parent().map(Path::to_path_buf).unwrap_or_default();
        SessionStore { dir, current: s }
    });
    let join = tokio::spawn(async move {
        runtime_loop(app_id, agent, session_store, cmd_rx, subscribers_task).await;
    });
    AppHandle {
        id: app_id,
        cmd_tx,
        subscribers,
        join,
        slash_commands,
        model,
        root,
        history,
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
            Ok(next) => store.current = next,
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

async fn runtime_loop(
    app_id: AppId,
    mut agent: Agent,
    mut session_store: Option<SessionStore>,
    mut cmd_rx: mpsc::UnboundedReceiver<AppCmd>,
    subscribers: Subscribers,
) {
    // Leading in-memory messages already written to the store. On `/clear`
    // the in-memory history is replaced, so this resets to 0 while the store
    // itself is kept untouched; later turns append after the old content.
    let mut persisted_prefix = agent.history().len();
    let mut persisted_rev = agent.history_revision();

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

                while let Ok(event) = agent_rx.try_recv() {
                    if matches!(&event, AgentEvent::HistoryCleared { .. }) {
                        switch_session(&mut session_store, &subscribers, app_id);
                    }
                    emit(&subscribers, AppEvent::Agent { app_id, event });
                }

                match result {
                    Ok(_) => {
                        if let Some(store) = &session_store {
                            let rev = agent.history_revision();
                            let after = agent.history();
                            if rev != persisted_rev {
                                // History was replaced in memory (`/clear`):
                                // keep the store untouched and treat the new
                                // in-memory chat as unpersisted.
                                persisted_prefix = 0;
                                persisted_rev = rev;
                            } else if after.len() > persisted_prefix {
                                if let Err(e) = store.current.append_all(&after[persisted_prefix..])
                                {
                                    emit(
                                        &subscribers,
                                        AppEvent::Error {
                                            app_id,
                                            message: e.to_string(),
                                        },
                                    );
                                } else {
                                    persisted_prefix = after.len();
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

    #[tokio::test]
    async fn spawn_prompt_emits_done_and_idle() {
        let tmp = tempdir::TempDir::new("app-runtime").unwrap();
        let app = App::new(tmp.path());
        let mock = MockProvider::new(vec![text_response("hello")]);
        let handle = app.spawn_with_provider(Box::new(mock)).await.unwrap();

        let mut rx = handle.subscribe();
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
        assert_eq!(names, ["clear", "exit"]);
        assert!(handle.slash_commands().iter().all(|(_, d)| !d.is_empty()));

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
}

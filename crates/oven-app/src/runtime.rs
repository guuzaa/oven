use std::path::Path;

use oven_agent::{Agent, AgentEvent, Cancel};
use oven_llm::{Provider, Role};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::session::Session;
use crate::{App, AppError};

/// Id for one long-lived oven-app instance inside a TUI process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AppId(pub u64);

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

/// Handle to a running oven-app actor.
pub struct AppHandle {
    id: AppId,
    cmd_tx: mpsc::UnboundedSender<AppCmd>,
    event_tx: broadcast::Sender<AppEvent>,
    join: JoinHandle<()>,
}

impl AppHandle {
    pub fn id(&self) -> AppId {
        self.id
    }

    pub fn send(&self, cmd: AppCmd) -> Result<(), AppError> {
        self.cmd_tx.send(cmd).map_err(|_| AppError::ChannelClosed)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.event_tx.subscribe()
    }

    /// Send one user turn and wait until the app returns to [`AppEvent::Idle`].
    /// Returns the final assistant text from [`AgentEvent::Done`] when present.
    pub async fn prompt(&self, input: impl Into<String>) -> Result<String, AppError> {
        let mut rx = self.subscribe();
        self.send(AppCmd::UserInput(input.into()))?;

        let mut text = String::new();
        loop {
            match rx.recv().await {
                Ok(AppEvent::Agent {
                    event: AgentEvent::Done { text: t, .. },
                    ..
                }) => text = t,
                Ok(AppEvent::Idle { .. }) => return Ok(text),
                Ok(AppEvent::Error { message, .. }) => {
                    return Err(AppError::Runtime(message));
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(AppError::ChannelClosed);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
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
    pub fn spawn(&self) -> AppHandle {
        let agent = self.build_agent();
        spawn_runtime(AppId::next(), agent, None)
    }

    /// Spawn with JSONL session under the platform data dir.
    pub fn spawn_session(&self, session_id: &str) -> Result<AppHandle, AppError> {
        let Some(dir) = crate::session::default_sessions_dir() else {
            return Err(AppError::Session(crate::session::SessionError::Io(
                std::path::PathBuf::from("<data_dir>"),
                std::io::Error::new(std::io::ErrorKind::NotFound, "no data_dir on this platform"),
            )));
        };
        self.spawn_session_in(&dir, session_id)
    }

    /// Spawn with JSONL session under an explicit directory.
    pub fn spawn_session_in(
        &self,
        sessions_dir: &Path,
        session_id: &str,
    ) -> Result<AppHandle, AppError> {
        let session = Session::open(sessions_dir, session_id)?;
        let prior = session.load()?;
        let mut agent = self.build_agent();
        for m in prior.into_iter().filter(|m| m.role != Role::System) {
            agent.push_history(m);
        }
        Ok(spawn_runtime(AppId::next(), agent, Some(session)))
    }

    /// Spawn with an explicit provider (tests / custom wiring).
    pub fn spawn_with_provider(&self, provider: Box<dyn Provider>) -> AppHandle {
        let agent = self.build_agent_with_provider(provider);
        spawn_runtime(AppId::next(), agent, None)
    }

    /// Spawn with provider + session store (tests).
    pub fn spawn_with_provider_session(
        &self,
        provider: Box<dyn Provider>,
        session: Session,
    ) -> Result<AppHandle, AppError> {
        let prior = session.load()?;
        let mut agent = self.build_agent_with_provider(provider);
        for m in prior.iter().filter(|m| m.role != Role::System) {
            agent.push_history(m.clone());
        }
        Ok(spawn_runtime(AppId::next(), agent, Some(session)))
    }
}

impl AppId {
    fn next() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

fn spawn_runtime(app_id: AppId, agent: Agent, session: Option<Session>) -> AppHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (event_tx, _) = broadcast::channel(256);
    let event_tx_task = event_tx.clone();
    let join = tokio::spawn(async move {
        runtime_loop(app_id, agent, session, cmd_rx, event_tx_task).await;
    });
    AppHandle {
        id: app_id,
        cmd_tx,
        event_tx,
        join,
    }
}

fn emit(tx: &broadcast::Sender<AppEvent>, event: AppEvent) {
    let _ = tx.send(event);
}

async fn runtime_loop(
    app_id: AppId,
    mut agent: Agent,
    session: Option<Session>,
    mut cmd_rx: mpsc::UnboundedReceiver<AppCmd>,
    event_tx: broadcast::Sender<AppEvent>,
) {
    let mut persisted_len = agent.history().len();

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            AppCmd::Shutdown => break,
            AppCmd::Cancel => {
                // no in-flight turn
            }
            AppCmd::UserInput(input) => {
                let cancel = Cancel::new();
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
                                    Some(AppCmd::UserInput(_)) => {}
                                }
                            }
                            ev = agent_rx.recv() => {
                                match ev {
                                    Some(event) => emit(
                                        &event_tx,
                                        AppEvent::Agent { app_id, event },
                                    ),
                                    None => break turn.await,
                                }
                            }
                            res = &mut turn => break res,
                        }
                    }
                };

                while let Ok(event) = agent_rx.try_recv() {
                    emit(&event_tx, AppEvent::Agent { app_id, event });
                }

                match result {
                    Ok(_) => {
                        if let Some(store) = &session {
                            let after = agent.history();
                            if after.len() > persisted_len {
                                if let Err(e) = store.append_all(&after[persisted_len..]) {
                                    emit(
                                        &event_tx,
                                        AppEvent::Error {
                                            app_id,
                                            message: e.to_string(),
                                        },
                                    );
                                } else {
                                    persisted_len = after.len();
                                }
                            }
                        }
                    }
                    Err(e) if e.is_cancelled() => {}
                    Err(e) => {
                        emit(
                            &event_tx,
                            AppEvent::Error {
                                app_id,
                                message: e.to_string(),
                            },
                        );
                    }
                }

                emit(&event_tx, AppEvent::Idle { app_id });
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
        let handle = app.spawn_with_provider(Box::new(mock));

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
    async fn session_persists_across_spawns() {
        let tmp = tempdir::TempDir::new("app-runtime-sess").unwrap();
        let app = App::new(tmp.path());
        let dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&dir).unwrap();

        let mock1 = MockProvider::new(vec![text_response("one")]);
        let session = Session::open(&dir, "s1").unwrap();
        let handle = app
            .spawn_with_provider_session(Box::new(mock1), session)
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
            .unwrap();
        assert_eq!(handle.prompt("second").await.unwrap(), "two");
        handle.shutdown().await;

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
        let handle = app.spawn_with_provider(Box::new(provider));
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
                Ok(Ok(AppEvent::Idle { .. })) => {
                    saw_idle = true;
                    break;
                }
                Ok(Ok(AppEvent::Error { .. })) => saw_error = true,
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
                Err(_) => panic!("timeout waiting for idle after cancel"),
            }
        }
        assert!(saw_idle);
        assert!(!saw_error);
        drop(tx);
        handle.shutdown().await;
    }
}

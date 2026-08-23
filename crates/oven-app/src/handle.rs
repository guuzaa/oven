use std::path::{Path, PathBuf};

use oven_agent::{AgentEvent, TodoList, TurnEvent};
use oven_llm::{Message, Usage};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::AppError;
use crate::command::AppCommand;
use crate::config::ProviderConfig;
use crate::event::{AppEvent, AppEventKind, AppId, Subscribers};
use crate::state::AppState;

pub struct AppHandle {
    id: AppId,
    cmd_tx: mpsc::UnboundedSender<AppCommand>,
    subscribers: Subscribers,
    join: JoinHandle<()>,
    slash_commands: Vec<(String, String)>,
    root: PathBuf,
    state: watch::Receiver<AppState>,
}

impl AppHandle {
    pub(crate) fn new(
        id: AppId,
        cmd_tx: mpsc::UnboundedSender<AppCommand>,
        subscribers: Subscribers,
        join: JoinHandle<()>,
        slash_commands: Vec<(String, String)>,
        root: PathBuf,
        state: watch::Receiver<AppState>,
    ) -> Self {
        Self {
            id,
            cmd_tx,
            subscribers,
            join,
            slash_commands,
            root,
            state,
        }
    }

    pub fn id(&self) -> AppId {
        self.id
    }

    pub fn send(&self, cmd: AppCommand) -> Result<(), AppError> {
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

    pub fn slash_commands(&self) -> &[(String, String)] {
        &self.slash_commands
    }

    pub fn state(&self) -> AppState {
        self.state.borrow().clone()
    }

    pub fn watch_state(&self) -> watch::Receiver<AppState> {
        self.state.clone()
    }

    pub fn model(&self) -> String {
        self.state.borrow().model.clone()
    }

    pub fn provider_config(&self) -> ProviderConfig {
        self.state.borrow().provider.clone()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn history(&self) -> Vec<Message> {
        self.state.borrow().history.clone()
    }

    pub fn todos(&self) -> TodoList {
        self.state.borrow().todos.clone()
    }

    pub fn total_usage(&self) -> Usage {
        self.state.borrow().usage
    }

    pub fn session_id(&self) -> Option<String> {
        self.state.borrow().session.id.clone()
    }

    pub async fn prompt(&self, input: impl Into<String>) -> Result<String, AppError> {
        let mut rx = self.subscribe();
        self.send(AppCommand::StartTurn {
            input: input.into(),
        })?;

        let mut text = String::new();
        let mut in_turn = false;
        loop {
            match rx.recv().await {
                Some(AppEvent {
                    kind: AppEventKind::Agent(env),
                    ..
                }) => match env.event {
                    AgentEvent::Turn(TurnEvent::Started) => {
                        in_turn = true;
                        text.clear();
                    }
                    AgentEvent::Stream(oven_agent::StreamEvent::TextDelta { text: t }) => {
                        text.push_str(&t);
                    }
                    AgentEvent::Turn(TurnEvent::Completed { .. }) => return Ok(text),
                    AgentEvent::Turn(TurnEvent::Cancelled) => return Ok(text),
                    AgentEvent::Turn(TurnEvent::Failed { error }) => {
                        return Err(AppError::Runtime(error.message));
                    }
                    _ => {}
                },
                Some(AppEvent {
                    kind: AppEventKind::Notification { text: t },
                    ..
                }) if !in_turn => return Ok(t),
                Some(AppEvent {
                    kind: AppEventKind::Error { message },
                    ..
                }) => return Err(AppError::Runtime(message)),
                Some(AppEvent {
                    kind: AppEventKind::Exited,
                    ..
                }) if !in_turn => {
                    return Ok(if text.is_empty() {
                        "goodbye".into()
                    } else {
                        text
                    });
                }
                Some(_) => {}
                None => return Err(AppError::ChannelClosed),
            }
        }
    }

    pub async fn shutdown(self) {
        let _ = self.cmd_tx.send(AppCommand::Shutdown);
        let _ = self.join.await;
    }
}

use std::path::{Path, PathBuf};
use std::string::String;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError};

use oven_agent::AgentError;
use oven_agent::{AgentEvent, LoopLimitDecision, TodoList, TurnEvent};
use oven_llm::{Message, Usage};
use thiserror::Error;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::builder::AppBuilder;
use crate::command::{AppCommand, ControlCommand};
use crate::config::{ConfigError, ProviderConfig};
use crate::event::{AppEvent, AppEventKind, AppId, ShellEvent, Subscribers};
use crate::session::SessionError;
use crate::state::AppState;

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("app channel closed")]
    ChannelClosed,
    /// A command asked for the conversation driver while a turn holds it.
    /// The runtime reads this as "defer the command", not as a failure.
    #[error("the agent is busy with a running turn")]
    AgentBusy,
    #[error("{0}")]
    Runtime(String),
    #[error("provider: {0}")]
    Provider(String),
    #[error("mcp: {0}")]
    Mcp(String),
}

impl From<oven_llm::ProviderError> for AppError {
    fn from(err: oven_llm::ProviderError) -> Self {
        match &err {
            oven_llm::ProviderError::InvalidRequest(reason) => {
                AppError::Provider(format!("invalid request: {reason}"))
            }
            _ => AppError::Provider(err.to_string()),
        }
    }
}

pub struct App {
    id: AppId,
    cmd_tx: mpsc::UnboundedSender<AppCommand>,
    subscribers: Subscribers,
    join: JoinHandle<()>,
    /// Queued prompts the runtime never got to run, filled in as it shuts
    /// down so a frontend can report them once it has the terminal back.
    unsent: Arc<AtomicUsize>,
    slash_commands: Vec<(String, String)>,
    root: PathBuf,
    state: watch::Receiver<AppState>,
}

impl App {
    pub fn builder(root: impl Into<PathBuf>) -> AppBuilder {
        AppBuilder::new(root)
    }

    pub async fn open(root: impl Into<PathBuf>) -> Result<Self, AppError> {
        let mut builder = Self::builder(root);
        builder.load_config()?;
        builder.open().await
    }

    pub async fn query(
        root: impl Into<PathBuf>,
        prompt: impl Into<String>,
    ) -> Result<String, AppError> {
        let app = Self::open(root).await?;
        let out = app.prompt(prompt).await;
        app.shutdown().await;
        out
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the handle is assembled once from the pieces spawn_runtime already holds"
    )]
    pub(crate) fn new(
        id: AppId,
        cmd_tx: mpsc::UnboundedSender<AppCommand>,
        subscribers: Subscribers,
        join: JoinHandle<()>,
        slash_commands: Vec<(String, String)>,
        root: PathBuf,
        state: watch::Receiver<AppState>,
        unsent: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            id,
            cmd_tx,
            subscribers,
            join,
            unsent,
            slash_commands,
            root,
            state,
        }
    }

    pub fn id(&self) -> AppId {
        self.id
    }

    /// The conversation driver. Anything else a view sees is a subagent.
    pub fn agent_id(&self) -> oven_agent::AgentId {
        self.state.borrow().agent_id
    }

    /// Subagents in spawn order, mirrored from the registry.
    pub fn subagents(&self) -> Vec<oven_agent::NodeInfo> {
        self.state.borrow().subagents.as_ref().clone()
    }

    pub fn send(&self, cmd: AppCommand) -> Result<(), AppError> {
        self.cmd_tx.send(cmd).map_err(|_| AppError::ChannelClosed)
    }

    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<AppEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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

    pub fn configured_providers(&self) -> Vec<String> {
        self.state.borrow().configured_providers.clone()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The conversation with its record timestamps and thinking durations,
    /// sharing the agent's messages so a transcript re-seed does not copy
    /// every message.
    pub fn history_timed_shared(&self) -> Vec<(Arc<Message>, u64, Option<u64>)> {
        let state = self.state.borrow();
        state
            .history
            .iter()
            .cloned()
            .zip(state.history_timestamps.iter().copied())
            .zip(state.history_thinking_ms.iter().copied())
            .map(|((message, timestamp), thinking_ms)| (message, timestamp, thinking_ms))
            .collect()
    }

    pub fn todos(&self) -> TodoList {
        self.state.borrow().todos.clone()
    }

    pub fn last_turn_usage(&self) -> Usage {
        self.state.borrow().last_turn_usage
    }

    pub fn session_id(&self) -> Option<String> {
        self.state.borrow().session.id.clone()
    }

    pub async fn prompt(&self, input: impl Into<String>) -> Result<String, AppError> {
        let mut rx = self.subscribe();
        let main = self.state().agent_id;
        self.send(AppCommand::Prompt(input.into()))?;

        let mut text = String::new();
        let mut in_turn = false;
        loop {
            match rx.recv().await {
                Some(AppEvent {
                    kind: AppEventKind::Agent(env),
                    ..
                }) if env.agent_id == main => match env.event {
                    AgentEvent::Turn(TurnEvent::Started) => {
                        in_turn = true;
                        text.clear();
                    }
                    AgentEvent::Stream(oven_agent::StreamEvent::TextDelta { text: t }) => {
                        text.push_str(&t);
                    }
                    AgentEvent::Turn(TurnEvent::Completed { .. } | TurnEvent::Cancelled { .. }) => {
                        return Ok(text);
                    }
                    AgentEvent::Turn(TurnEvent::Failed { error, .. }) => {
                        return Err(AppError::Runtime(error.message));
                    }
                    AgentEvent::Turn(TurnEvent::LoopLimitReached { request_id, .. }) => {
                        let _ = self.send(AppCommand::Control(ControlCommand::RespondLoopLimit {
                            request_id,
                            decision: LoopLimitDecision::Exit,
                        }));
                    }
                    _ => {}
                },
                Some(AppEvent {
                    kind: AppEventKind::Shell(ev),
                    ..
                }) => match ev {
                    ShellEvent::Started { .. } => {
                        in_turn = true;
                    }
                    ShellEvent::Finished { output, .. } => return Ok(output),
                    ShellEvent::Failed { error, output, .. } => {
                        return Err(AppError::Runtime(if output.is_empty() {
                            error
                        } else {
                            output
                        }));
                    }
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

    /// Shuts the runtime down and reports how many queued prompts were
    /// dropped without ever running, so a frontend can say so.
    pub async fn shutdown(self) -> usize {
        let _ = self.cmd_tx.send(AppCommand::Shutdown);
        let _ = self.join.await;
        self.unsent.load(Ordering::Relaxed)
    }
}

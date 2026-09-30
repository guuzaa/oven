use std::path::{Path, PathBuf};
use std::string::String;
use std::sync::{Arc, PoisonError};

use oven_agent::{
    AgentEvent, AgentId, AgentMode, LoopLimitDecision, TodoList, TurnEvent, TurnId, UserRequestId,
    UserResponse,
};
use oven_llm::{Message, Usage};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::api::builder::AppBuilder;
use crate::api::input::Input;
use crate::commands::SlashRegistry;
use crate::core::config::ProviderConfig;
use crate::core::error::AppError;
use crate::core::event::{AppEvent, AppEventKind, AppId, ShellEvent, Subscribers};
use crate::core::state::AppState;
use crate::runtime::inbox::InboxSender;
use crate::runtime::shared::{Shared, queued_notice};

pub struct App {
    id: AppId,
    inbox: InboxSender,
    subscribers: Subscribers,
    join: JoinHandle<()>,
    slash: Arc<SlashRegistry>,
    root: PathBuf,
    shared: Arc<Shared>,
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

    pub(crate) fn new(
        id: AppId,
        inbox: InboxSender,
        subscribers: Subscribers,
        join: JoinHandle<()>,
        slash: Arc<SlashRegistry>,
        root: PathBuf,
        shared: Arc<Shared>,
    ) -> Self {
        Self {
            id,
            inbox,
            subscribers,
            join,
            slash,
            root,
            state: shared.state.subscribe(),
            shared,
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

    /// Classifies `text` and hands it to the runtime, returning what it was
    /// taken for so a frontend can draw it the same way.
    pub fn submit(&self, text: impl AsRef<str>) -> Result<Input, AppError> {
        let input = Input::parse(text.as_ref(), &self.slash);
        self.dispatch(input.clone())?;
        Ok(input)
    }

    /// Drops the last turn from the conversation once the driver is free.
    pub fn rewind(&self) -> Result<(), AppError> {
        self.dispatch(Input::Rewind)
    }

    /// Applies an input on the spot if it needs no driver while one is busy;
    /// otherwise queues it for the driver, saying so when the user will
    /// have to wait.
    fn dispatch(&self, input: Input) -> Result<(), AppError> {
        if self.shared.is_busy() {
            if self.shared.apply_now(&input, &self.slash) {
                return Ok(());
            }
            if let Some(text) = queued_notice(&input) {
                self.shared.notify(text);
            }
        }
        self.inbox.send(input)
    }

    /// Cancels `turn_id` if it is still the running turn.
    pub fn cancel(&self, turn_id: TurnId) {
        self.shared.cancel(turn_id);
    }

    pub fn set_mode(&self, mode: AgentMode) {
        self.shared.set_mode(mode);
    }

    /// Answers the request the running turn is waiting on.
    pub fn respond(&self, request_id: UserRequestId, response: UserResponse) {
        self.shared.respond(request_id, response);
    }

    pub fn stop_subagent(&self, id: AgentId) {
        self.shared.stop_subagent(id);
    }

    pub fn stop_subagents(&self) {
        self.shared.stop_subagents();
    }

    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<AppEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(tx);
        rx
    }

    pub fn slash_commands(&self) -> Vec<(String, String)> {
        self.slash.commands()
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

    pub fn history_timed_shared(&self) -> Vec<(Arc<Message>, u64, Option<u64>)> {
        self.state.borrow().history_timed_shared()
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
        self.submit(input.into())?;

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
                        self.respond(request_id, UserResponse::LoopLimit(LoopLimitDecision::Exit));
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
        self.shared.shutdown.cancel();
        let _ = self.join.await;
        self.inbox.unsent()
    }
}

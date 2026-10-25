use std::path::{Path, PathBuf};
use std::string::String;
use std::sync::{Arc, Mutex, PoisonError};

use oven_llm::{Message, Usage};
use oven_proto::{
    AgentId, AgentMode, AppEvent, AppId, AppState, Command, CommandResult, Input, NodeInfo,
    ProviderView, TodoList, TurnId, UserRequestId, UserResponse,
};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::api::builder::AppBuilder;
use crate::api::input;
use crate::client::Headless;
use crate::commands::SlashRegistry;
use crate::core::error::AppError;
use crate::core::event::Subscribers;
use crate::runtime::inbox::InboxSender;
use crate::runtime::shared::{Shared, queued_notice};

/// The handle a frontend talks to.
///
/// Cloneable, so a server can hand the same session to several
/// connections; the runtime behind it is shut down once, by whoever owns
/// the last handle.
#[derive(Clone)]
pub struct App {
    id: AppId,
    inbox: InboxSender,
    subscribers: Subscribers,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
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
        builder.load_config().await?;
        builder.load_memory().await;
        builder.open().await
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
            join: Arc::new(Mutex::new(Some(join))),
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
    pub fn agent_id(&self) -> AgentId {
        self.state.borrow().levels.agent_id
    }

    /// Subagents in spawn order, mirrored from the registry.
    pub fn subagents(&self) -> Vec<NodeInfo> {
        self.state.borrow().levels.subagents.as_ref().clone()
    }

    /// Classifies `text` the way the runtime will: a chat, a `!` shell line
    /// or a slash command.
    ///
    /// The app owns the slash registry, so a frontend asks it rather than
    /// reading the text itself. A submission is classified once: a message
    /// that was parked and taken back is submitted as it was classified.
    pub fn classify(&self, text: &str) -> Input {
        input::classify(text, &self.slash)
    }

    /// Queues an input for the driver: it runs as a turn of its own.
    ///
    /// The input is returned as it was queued, so a frontend draws it the
    /// way the runtime will treat it.
    pub async fn submit(&self, input: Input) -> Result<Input, AppError> {
        self.dispatch(input.clone()).await?;
        Ok(input)
    }

    /// Parks a chat with the running turn, so the turn appends it to its own
    /// history instead of running it as a turn of its own.
    ///
    /// `false` when the driver is idle, or when the input is not a chat.
    pub fn steer(&self, input: &Input) -> bool {
        let Input::Chat(text) = input else {
            return false;
        };
        if !self.shared.is_busy() {
            return false;
        }
        self.shared.push_prompt(text.clone());
        true
    }

    /// Takes a parked chat back. `false` when the running turn already took
    /// it.
    pub fn unpark(&self, text: &str) -> bool {
        self.shared.claim_prompt(text)
    }

    /// Drops the last turn from the conversation once the driver is free.
    pub async fn rewind(&self) -> Result<(), AppError> {
        self.dispatch(Input::Rewind).await
    }

    /// Applies an input on the spot if it needs no driver while one is busy;
    /// otherwise queues it for the driver, saying so when the user will
    /// have to wait.
    async fn dispatch(&self, input: Input) -> Result<(), AppError> {
        if self.shared.is_busy() {
            if self.shared.apply_now(&input, &self.slash).await {
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

    /// Aborts a compaction in progress. A second call while it is already
    /// stopping, or a call when nothing is compacting, does nothing.
    pub fn cancel_compaction(&self) {
        self.shared.cancel_compaction();
    }

    pub fn set_mode(&self, mode: AgentMode) {
        self.shared.set_mode(mode);
    }

    /// Answers the request the running turn is waiting on. `false` when the
    /// request was already closed, or the answer did not fit it.
    pub fn respond(&self, request_id: UserRequestId, response: UserResponse) -> bool {
        self.shared.respond(request_id, response)
    }

    pub fn stop_subagent(&self, id: AgentId) {
        self.shared.stop_subagent(id);
    }

    pub fn stop_subagents(&self) {
        self.shared.stop_subagents();
    }

    /// Subscribes to the events a frontend can see.
    ///
    /// The event is shared, not copied: a subscriber reads the same event
    /// the bus published, so a stream of text deltas and a tool result cost
    /// one reference count rather than a copy each.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<Arc<AppEvent>> {
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
        self.state.borrow().levels.model.clone()
    }

    /// The active provider as a frontend may see it: the credential stays in
    /// the app layer, so a frontend is told whether one is configured, never
    /// which.
    pub fn provider_view(&self) -> ProviderView {
        self.state.borrow().levels.provider.clone()
    }

    pub fn configured_providers(&self) -> Vec<String> {
        self.state.borrow().levels.configured_providers.clone()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn history_timed_shared(&self) -> Vec<(Arc<Message>, u64, Option<u64>)> {
        self.state.borrow().history_timed_shared()
    }

    pub fn todos(&self) -> TodoList {
        self.state.borrow().levels.todos.clone()
    }

    pub fn last_turn_usage(&self) -> Usage {
        self.state.borrow().levels.last_turn_usage
    }

    pub fn session_id(&self) -> Option<String> {
        self.state.borrow().levels.session.id.clone()
    }

    /// Asks the app something and waits for the answer.
    ///
    /// The answer is read off the event stream by a [`Headless`] client, so
    /// a frontend that wants the same thing builds one instead of writing
    /// the loop again.
    pub async fn prompt(&self, input: impl Into<String>) -> Result<String, AppError> {
        Headless::new(self.clone()).ask(&input.into()).await
    }

    pub(crate) async fn wait_until_idle(&self) {
        let mut state = self.state.clone();
        loop {
            if state.borrow().levels.phase.is_idle() {
                return;
            }
            if state.changed().await.is_err() {
                return;
            }
        }
    }

    /// Applies one protocol command. The single entry a frontend needs: a
    /// transport turns what it received into a [`Command`] and hands it here.
    pub async fn apply(&self, command: Command) -> Result<CommandResult, AppError> {
        let outcome = match command {
            Command::Submit { input } => CommandResult::Submitted(self.submit(input).await?),
            Command::Steer { input } => {
                if self.steer(&input) {
                    CommandResult::Parked(input)
                } else {
                    CommandResult::Refused
                }
            }
            Command::Unpark { text } => {
                if self.unpark(&text) {
                    CommandResult::Unparked(Input::Chat(text))
                } else {
                    CommandResult::Refused
                }
            }
            Command::Rewind => {
                self.rewind().await?;
                CommandResult::Applied
            }
            Command::Cancel { turn_id } => {
                self.cancel(turn_id);
                CommandResult::Applied
            }
            Command::CancelCompaction => {
                self.cancel_compaction();
                CommandResult::Applied
            }
            Command::SetMode { mode } => {
                self.set_mode(mode);
                CommandResult::Applied
            }
            Command::Respond {
                request_id,
                response,
            } => {
                if self.respond(request_id, response) {
                    CommandResult::Applied
                } else {
                    CommandResult::Refused
                }
            }
            Command::StopSubagent { id } => {
                self.stop_subagent(id);
                CommandResult::Applied
            }
            Command::StopSubagents => {
                self.stop_subagents();
                CommandResult::Applied
            }
        };
        Ok(outcome)
    }

    /// Shuts the runtime down and reports how many queued prompts were
    /// dropped without ever running, so a frontend can say so.
    pub async fn shutdown(&self) -> usize {
        self.shared.shutdown.cancel();
        let join = self
            .join
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(join) = join {
            let _ = join.await;
        }
        self.inbox.unsent()
    }
}

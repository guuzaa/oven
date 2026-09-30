//! The conversation driver.
//!
//! An `Agent` holds its tools, its history and the mode and model its next
//! step runs with. What one step does sits beside it: `request` builds the
//! provider request and streams the reply back, `loop` runs the step and the
//! tools it asked for, and `notify` times the thinking window and logs every
//! call.

use std::sync::{Arc, PoisonError, RwLock};

use oven_llm::{Message, ModelId, ReasoningEffort, Router};

use crate::capabilities::tools::Tool;
use crate::core::event::AgentEvent;
use crate::core::history::{History, Record};
use crate::core::identity::AgentId;
use crate::core::mode::AgentMode;
use crate::core::selection::Selection;
use crate::core::sink::EventSink;
use crate::core::todo::TodoList;

mod notify;
mod request;
mod step;

/// A router shared between an `Agent` and callers that need to read it
/// (e.g. to validate a model switch) without the exclusive `&mut Agent`
/// access a running turn holds. Reading clones the inner `Arc<Router>`
/// snapshot (cheap, safe to hold across `.await`); mutating goes through
/// [`Agent::replace_router`].
pub type RouterHandle = Arc<RwLock<Arc<Router>>>;

/// A handle onto `router`, for callers that need the conversation driver and
/// the agents it spawns to share one router from the start.
pub fn router_handle(router: Router) -> RouterHandle {
    Arc::new(RwLock::new(Arc::new(router)))
}

/// The conversation driver. Holds tools and dispatches tool calls returned by
/// the provider until the provider replies without tool calls.
pub struct Agent {
    id: AgentId,
    router: RouterHandle,
    pub(crate) tools: Vec<Arc<dyn Tool>>,
    pub(crate) history: History,
    selection: Selection,
    system: Option<String>,
    todos: TodoList,
    todo_written_this_turn: bool,
    todo_dirty: bool,
}

impl Agent {
    pub fn new(router: Router, tools: Vec<Arc<dyn Tool>>) -> Self {
        Self::with_router(router_handle(router), tools)
    }

    /// Build an agent on a router it shares with whoever handed the handle
    /// over. A subagent joins the conversation driver's router rather than
    /// snapshotting it, so a `/setup` or `/model` switch reaches both.
    pub fn with_router(router: RouterHandle, tools: Vec<Arc<dyn Tool>>) -> Self {
        Self {
            id: AgentId::next(),
            router,
            tools,
            history: History::new(),
            selection: Selection::new(AgentMode::Agent, ModelId::new("default"), None),
            system: None,
            todos: TodoList::default(),
            todo_written_this_turn: false,
            todo_dirty: false,
        }
    }

    pub fn with_id(mut self, id: AgentId) -> Self {
        self.id = id;
        self
    }

    pub fn id(&self) -> AgentId {
        self.id
    }

    pub fn with_model(self, model: impl Into<ModelId>) -> Self {
        self.selection.set_model(model.into());
        self
    }

    pub fn model(&self) -> ModelId {
        self.selection.model().0
    }

    pub fn with_system(mut self, content: impl Into<String>) -> Self {
        self.system = Some(content.into());
        self
    }

    pub fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.selection.model().1
    }

    /// A handle onto the mode and model the next step runs with, independent
    /// of `&Agent`/`&mut Agent`. Changing it while a turn runs takes effect
    /// at the turn's next step.
    pub fn selection(&self) -> Selection {
        self.selection.clone()
    }

    /// A snapshot of the current router. Cheap to clone and safe to hold
    /// across `.await` points, unlike a lock guard.
    pub fn router(&self) -> Arc<Router> {
        self.router
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A handle to the shared router, independent of `&Agent`/`&mut Agent`.
    /// Lets a caller validate or read the router while a turn holds the
    /// agent's exclusive `&mut` borrow.
    pub fn router_handle(&self) -> RouterHandle {
        Arc::clone(&self.router)
    }

    /// Swaps in a freshly built router (e.g. `/setup` registering a new
    /// provider). Replacing the snapshot rather than mutating it in place is
    /// what makes this safe while another agent shares the same handle: a
    /// reader that captured the old router finishes its request on it, and
    /// the next reader gets the new one.
    pub fn replace_router(&mut self, router: Router) {
        let mut guard = self.router.write().unwrap_or_else(PoisonError::into_inner);
        *guard = Arc::new(router);
    }

    pub fn set_model(&mut self, model: impl Into<ModelId>) {
        self.selection.set_model(model.into());
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<ReasoningEffort>) {
        self.selection.set_reasoning_effort(effort);
    }

    pub fn set_mode(&mut self, mode: AgentMode) {
        self.selection.set_mode(mode);
    }

    pub fn mode(&self) -> AgentMode {
        self.selection.mode()
    }

    pub fn set_todos(&mut self, todos: TodoList) {
        self.todos = todos;
    }

    pub fn todos(&self) -> &TodoList {
        &self.todos
    }

    pub fn todo_written_this_turn(&self) -> bool {
        self.todo_written_this_turn
    }

    fn dismiss_finished_todos(&mut self, sink: &mut impl EventSink) {
        if !self.todos.is_finished() {
            return;
        }
        self.todos = TodoList::default();
        self.todo_written_this_turn = true;
        sink.emit(AgentEvent::TodosChanged {
            todos: TodoList::default(),
        });
    }

    /// Set the reasoning effort for provider calls.
    pub fn with_reasoning_effort(self, effort: ReasoningEffort) -> Self {
        self.selection.set_reasoning_effort(Some(effort));
        self
    }

    pub fn history(&self) -> impl ExactSizeIterator<Item = &Message> + '_ {
        self.history.messages()
    }

    /// The conversation as shared handles: a snapshot costs one refcount bump
    /// per message instead of copying every message the app layer renders.
    pub fn shared_history(&self) -> Vec<Arc<Message>> {
        self.history.shared_messages().cloned().collect()
    }

    pub fn history_timed(
        &self,
    ) -> impl ExactSizeIterator<Item = (&Message, u64, Option<u64>)> + '_ {
        self.history.iter_timed()
    }

    pub fn history_revision(&self) -> u64 {
        self.history.revision()
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
        self.todo_dirty = false;
    }

    /// Replace the entire history with records loaded from a persisted
    /// session (messages plus a `TokenUsage` record after each turn's final
    /// assistant message). The cumulative total is rebuilt from the restored
    /// usage.
    pub fn restore_history(&mut self, records: Vec<Record>) {
        self.history.set_messages_with_records(records);
    }

    /// Append a message to the history. Used by the App layer to preload a
    /// persisted session.
    pub fn push_history(&mut self, message: Message) {
        self.history.push(message);
    }

    /// Record the session's workspace root if it is not already known (a
    /// resumed session keeps its original root and creation time). Persisted
    /// as the first record of the session file.
    pub fn ensure_session_meta(&mut self, root: String) {
        self.history.ensure_session_meta(root);
    }

    /// The history as persistence-ready records: messages plus a `TokenUsage`
    /// record after each turn's final assistant message. The App layer
    /// persists these as JSONL lines.
    pub fn history_records(&self) -> Vec<Record> {
        self.history.records()
    }

    /// [`history_records`](Self::history_records) for the messages from index
    /// `start` onward. The App layer appends only these after each turn, so
    /// persisting never rescales with the whole conversation.
    pub fn history_records_from(&self, start: usize) -> Vec<Record> {
        self.history.records_from(start)
    }

    /// Remove the last user turn from the conversation history, returning
    /// the removed user message, or `None` when there is nothing to rewind.
    /// The removed turn's cumulative token usage is rolled back out of the
    /// running total.
    pub fn rewind_last_turn(&mut self) -> Option<Message> {
        let removed = self.history.rewind_last_turn();
        if removed.is_some() {
            self.todo_dirty = false;
        }
        removed
    }
}

#[cfg(test)]
#[path = "agent_test.rs"]
mod tests;

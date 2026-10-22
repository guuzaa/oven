use oven_app::AgentMode;

#[derive(Default)]
pub struct State {
    /// The conversation driver's own turn is running. A subagent working in
    /// the background does not set this: the composer stays free to send.
    pub busy: bool,
    /// Subagents still working, which keep the spinner turning and the
    /// strip's clocks moving.
    pub agents: usize,
    pub mode: AgentMode,
    pub frame: u64,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether anything at all is in flight: the driver's turn, or a
    /// subagent it delegated to.
    pub fn working(&self) -> bool {
        self.busy || self.agents > 0
    }
}

pub enum Action {
    Quit,
    Cancel,
    Submit(String),
    Queue(String),
    /// Run a slash command without touching the transcript or input.
    QuietSubmit(String),
    /// Show a transient status-bar notify (same path as a notification event).
    Notify(String),
}

pub enum KeyResult {
    Ignored,
    Handled,
    Action(Action),
}

#[cfg(test)]
pub(crate) fn idle_levels() -> oven_app::AppLevels {
    use std::sync::Arc;

    use oven_app::{AgentId, AppLevels, AppPhase, ProviderView, SessionState, TodoList};
    use oven_llm::Usage;

    AppLevels {
        phase: AppPhase::Idle,
        agent_id: AgentId(1),
        subagents: Arc::default(),
        mode: AgentMode::default(),
        model: String::new(),
        reasoning_effort: None,
        provider: ProviderView::default(),
        configured_providers: Vec::new(),
        todos: TodoList::default(),
        last_turn_usage: Usage::default(),
        context_tokens: 0,
        context_window: None,
        session: SessionState::default(),
        models: Vec::new(),
    }
}

#[cfg(test)]
pub(crate) fn app_event(event: oven_app::AgentEvent) -> oven_app::AppEvent {
    oven_app::AppEvent::agent_with(oven_app::AgentId(1), oven_app::TurnId(1), event)
}

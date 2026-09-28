use crossterm::event::{KeyEvent, MouseEvent};
use oven_app::{AgentMode, AppEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

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

pub trait Component {
    fn handle_key(&mut self, key: KeyEvent, state: &State) -> KeyResult;
    fn handle_mouse(&mut self, _mouse: MouseEvent, _state: &State) -> KeyResult {
        KeyResult::Ignored
    }
    fn on_event(&mut self, _ev: &AppEvent) {}
    fn draw(&mut self, f: &mut Frame<'_>, area: Rect, state: &State);
}

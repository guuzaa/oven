use crossterm::event::{KeyEvent, MouseEvent};
use oven_app::AppEvent;
use ratatui::Frame;
use ratatui::layout::Rect;

pub struct State {
    pub busy: bool,
}

impl State {
    pub fn new() -> Self {
        Self { busy: false }
    }
}

pub enum Action {
    Quit,
    Cancel,
    Submit(String),
    Queue(String),
    /// Run a slash command without touching the transcript or input.
    QuietSubmit(String),
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
    fn on_event(&mut self, _ev: &AppEvent, _state: &mut State) {}
    fn draw(&mut self, f: &mut Frame<'_>, area: Rect, state: &State);
}

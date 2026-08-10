use crossterm::event::KeyEvent;
use oven_agent::AgentEvent;
use oven_app::AppEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::component::{Component, KeyResult, State};
use super::tool_display;

pub struct StatusBar {
    text: String,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            text: "ready".into(),
        }
    }

    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }
}

impl Component for StatusBar {
    fn handle_key(&mut self, _key: KeyEvent, _state: &State) -> KeyResult {
        KeyResult::Ignored
    }

    fn on_event(&mut self, ev: &AppEvent, state: &mut State) {
        match ev {
            AppEvent::Agent { event, .. } => match event {
                AgentEvent::ThinkingDelta { .. } => self.text = "thinking…".into(),
                AgentEvent::TextDelta { .. } => self.text = "streaming…".into(),
                AgentEvent::ToolStart { name, input, .. } => {
                    self.text = format!("tool: {}…", tool_display(name, input));
                }
                AgentEvent::ToolEnd { ok, .. } => {
                    self.text = if *ok {
                        "tool done".into()
                    } else {
                        "tool failed".into()
                    };
                }
                AgentEvent::Done { .. } => {}
                AgentEvent::Cancelled { .. } => self.text = "cancelled".into(),
                AgentEvent::Exit { .. } => {}
                AgentEvent::HistoryCleared { .. } => {}
            },
            AppEvent::Idle { .. } => {
                state.busy = false;
                if self.text == "streaming…"
                    || self.text == "thinking…"
                    || self.text.starts_with("tool:")
                    || self.text == "cancelled"
                {
                    self.text = "ready".into();
                }
            }
            AppEvent::Error { .. } => {
                self.text = "error".into();
            }
        }
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect, state: &State) {
        let style = if state.busy {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let para = Paragraph::new(Span::styled(format!(" {} ", self.text), style));
        f.render_widget(para, area);
    }
}

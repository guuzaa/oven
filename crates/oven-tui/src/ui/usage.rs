use crossterm::event::KeyEvent;
use oven_agent::AgentEvent;
use oven_app::AppEvent;
use oven_llm::Usage;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::component::{Component, KeyResult, State};

pub struct UsageBar {
    total: Usage,
}

impl UsageBar {
    pub fn new() -> Self {
        Self {
            total: Usage::default(),
        }
    }
}

impl Component for UsageBar {
    fn handle_key(&mut self, _key: KeyEvent, _state: &State) -> KeyResult {
        KeyResult::Ignored
    }

    fn on_event(&mut self, ev: &AppEvent, _state: &mut State) {
        if let AppEvent::Agent {
            event: AgentEvent::Done { usage, .. },
            ..
        } = ev
        {
            self.total = *usage;
        }
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect, _state: &State) {
        let text = format_usage(&self.total);
        let para = Paragraph::new(Span::styled(text, Style::default().fg(Color::DarkGray)));
        f.render_widget(para, area);
    }
}

fn format_usage(u: &Usage) -> String {
    let i = human(u.input_tokens);
    let o = human(u.output_tokens);
    let mut s = format!(" ↑{i} in · ↓{o} out");
    if u.cache_read_tokens > 0 {
        s.push_str(&format!(" · cache {}", human(u.cache_read_tokens)));
    }
    if u.reasoning_tokens > 0 {
        s.push_str(&format!(" · reasoning {}", human(u.reasoning_tokens)));
    }
    s
}

fn human(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

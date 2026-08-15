use std::path::Path;

use crossterm::event::KeyEvent;
use oven_app::{AgentEvent, AppEvent};
use oven_llm::{ReasoningEffort, Usage};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::component::{Component, KeyResult, State};
use super::theme;

const SPIN_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Single status row below the input: model · root · token usage.
pub struct StatusBar {
    model: String,
    effort: Option<ReasoningEffort>,
    root: String,
    total: Usage,
}

impl StatusBar {
    pub fn new(model: impl Into<String>, root: &Path, total: Usage) -> Self {
        Self {
            model: model.into(),
            effort: None,
            root: display_path(root),
            total,
        }
    }

    pub fn draw_bar(
        &mut self,
        f: &mut Frame<'_>,
        area: Rect,
        state: &State,
        popup: bool,
        spin: u8,
    ) {
        let gray = theme::dim();
        let mut spans = Vec::new();
        if state.busy {
            let ch = SPIN_FRAMES[(spin as usize) % SPIN_FRAMES.len()];
            spans.push(Span::styled(ch.to_string(), theme::accent()));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(self.model.clone(), theme::model()));
        spans.push(Span::styled(" · ", gray));
        spans.push(Span::styled(self.root.clone(), theme::path()));
        spans.extend(usage_spans(&self.total, gray));
        if let Some(effort) = self.effort {
            spans.push(Span::styled(" · ", gray));
            spans.push(Span::styled(format!("effort {effort}"), theme::effort()));
        }
        let hint = if popup {
            "tab fill · enter · esc"
        } else if state.busy {
            "esc cancel · enter queue"
        } else {
            "enter send · alt-enter newline · esc undo"
        };
        let max = area.width as usize;
        let hint_w = hint.width();
        let left = Line::from(spans);
        let left_w = left.width();
        let line = if hint_w + 1 < max && left_w + 1 + hint_w <= max {
            let pad = max - left_w - hint_w;
            let mut spans = left.spans;
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(hint.to_string(), gray));
            Line::from(spans)
        } else {
            truncate_line(left, max.saturating_sub(1))
        };
        f.render_widget(Paragraph::new(line), area);
    }
}

impl Component for StatusBar {
    fn handle_key(&mut self, _key: KeyEvent, _state: &State) -> KeyResult {
        KeyResult::Ignored
    }

    fn on_event(&mut self, ev: &AppEvent, state: &mut State) {
        match ev {
            AppEvent::Agent { event, .. } => match event {
                AgentEvent::Done { usage, .. } => {
                    self.total = *usage;
                }
                AgentEvent::HistoryCleared { .. } => {
                    self.total = Usage::default();
                }
                AgentEvent::ModelChanged {
                    model,
                    reasoning_effort,
                    ..
                } => {
                    self.model = model.clone();
                    self.effort = *reasoning_effort;
                }
                _ => {}
            },
            AppEvent::Idle { .. } => {
                state.busy = false;
            }
            AppEvent::ModelsUpdated { .. } => {}
            AppEvent::Error { .. } => {}
            AppEvent::Rewound { usage, .. } => {
                self.total = *usage;
            }
        }
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect, state: &State) {
        self.draw_bar(f, area, state, false, 0);
    }
}

/// Token-usage segment of the status row: empty while nothing has been
/// recorded (all-zero `Usage`), otherwise a separator plus the formatted
/// totals.
fn usage_spans(total: &Usage, gray: Style) -> Vec<Span<'static>> {
    if *total == Usage::default() {
        Vec::new()
    } else {
        vec![
            Span::styled(" · ", gray),
            Span::styled(format_usage(total), gray),
        ]
    }
}

fn format_usage(u: &Usage) -> String {
    let i = human(u.input_tokens);
    let o = human(u.output_tokens);
    let mut s = format!("{i} in · {o} out");
    s.push_str(&format!(" · {} cache", human(u.cache_read_tokens)));
    if u.reasoning_tokens > 0 {
        s.push_str(&format!(" · {} reasoning", human(u.reasoning_tokens)));
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

pub(crate) fn truncate_str(s: &str, max_width: usize) -> String {
    if s.width() <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if width + cw > max_width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        width += cw;
    }
    out.push('…');
    out
}

fn truncate_line<'a>(line: Line<'a>, max_width: usize) -> Line<'a> {
    let mut spans = Vec::new();
    let mut width = 0usize;
    for span in line.spans {
        let text = truncate_str(&span.content, max_width.saturating_sub(width));
        let span_width = text.width();
        if span_width > 0 {
            spans.push(Span::styled(text, span.style));
        }
        width += span_width;
        if width >= max_width {
            break;
        }
    }
    Line::from(spans)
}

/// Absolute path with `~` in place of the home directory, if it lives there.
fn display_path(path: &Path) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return path.display().to_string();
    };
    let home = Path::new(&home);
    let home = home.canonicalize().unwrap_or_else(|_| home.to_owned());
    display_path_with_home(path, &home)
}

fn display_path_with_home(path: &Path, home: &Path) -> String {
    if path == home {
        return "~".into();
    }
    if let Ok(rest) = path.strip_prefix(home) {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oven_app::{AgentId, AppId};

    fn agent_event(event: AgentEvent) -> AppEvent {
        AppEvent::Agent {
            app_id: AppId(1),
            event,
        }
    }

    #[test]
    fn idle_clears_busy() {
        let mut bar = StatusBar::new("m", Path::new("/tmp"), Usage::default());
        let mut state = State { busy: true };
        bar.on_event(&AppEvent::Idle { app_id: AppId(1) }, &mut state);
        assert!(!state.busy);
    }

    #[test]
    fn history_cleared_resets_usage() {
        let mut bar = StatusBar::new("m", Path::new("/tmp"), Usage::default());
        let mut state = State::new();
        bar.total = Usage {
            input_tokens: 1000,
            output_tokens: 2000,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
        };
        bar.on_event(
            &agent_event(AgentEvent::HistoryCleared {
                agent_id: AgentId(1),
            }),
            &mut state,
        );
        assert_eq!(bar.total, Usage::default());
    }

    #[test]
    fn done_updates_token_usage() {
        let mut bar = StatusBar::new("m", Path::new("/tmp"), Usage::default());
        let mut state = State::new();
        let usage = Usage {
            input_tokens: 1234,
            output_tokens: 56,
            cache_read_tokens: 789,
            reasoning_tokens: 10,
        };
        bar.on_event(
            &agent_event(AgentEvent::Done {
                agent_id: AgentId(1),
                text: "done".into(),
                usage,
            }),
            &mut state,
        );
        assert_eq!(bar.total, usage);
    }

    #[test]
    fn rewound_syncs_token_usage() {
        let mut bar = StatusBar::new("m", Path::new("/tmp"), Usage::default());
        let mut state = State::new();
        bar.total = Usage {
            input_tokens: 1000,
            output_tokens: 2000,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
        };
        let usage = Usage {
            input_tokens: 500,
            output_tokens: 100,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
        };
        bar.on_event(
            &AppEvent::Rewound {
                app_id: AppId(1),
                text: Some("restored".into()),
                messages: Vec::new(),
                usage,
            },
            &mut state,
        );
        assert_eq!(bar.total, usage);
    }

    #[test]
    fn model_changed_updates_model_and_effort() {
        let mut bar = StatusBar::new("gpt-4o", Path::new("/tmp"), Usage::default());
        let mut state = State::new();
        bar.on_event(
            &agent_event(AgentEvent::ModelChanged {
                agent_id: AgentId(1),
                model: "deepseek-chat".into(),
                reasoning_effort: Some(ReasoningEffort::High),
            }),
            &mut state,
        );
        assert_eq!(bar.model, "deepseek-chat");
        assert_eq!(bar.effort, Some(ReasoningEffort::High));

        bar.on_event(
            &agent_event(AgentEvent::ModelChanged {
                agent_id: AgentId(1),
                model: "gpt-4o".into(),
                reasoning_effort: None,
            }),
            &mut state,
        );
        assert_eq!(bar.model, "gpt-4o");
        assert_eq!(bar.effort, None);
    }

    #[test]
    fn initial_usage_seeds_total() {
        let usage = Usage {
            input_tokens: 4200,
            output_tokens: 1300,
            cache_read_tokens: 900,
            reasoning_tokens: 0,
        };
        let bar = StatusBar::new("m", Path::new("/tmp"), usage);
        assert_eq!(bar.total, usage);
    }

    #[test]
    fn usage_hidden_while_default() {
        let gray = theme::dim();
        assert!(usage_spans(&Usage::default(), gray).is_empty());
    }

    #[test]
    fn usage_shown_once_recorded() {
        let gray = theme::dim();
        let usage = Usage {
            input_tokens: 1200,
            output_tokens: 30,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
        };
        let spans = usage_spans(&usage, gray);
        assert_eq!(spans.len(), 2);
        assert!(spans[1].content.as_ref().contains("1.2k in"));
    }

    #[test]
    fn home_dir_itself_becomes_tilde() {
        assert_eq!(
            display_path_with_home(Path::new("/home/u"), Path::new("/home/u")),
            "~"
        );
    }

    #[test]
    fn path_under_home_uses_tilde_prefix() {
        assert_eq!(
            display_path_with_home(Path::new("/home/u/code/oven"), Path::new("/home/u")),
            "~/code/oven"
        );
    }

    #[test]
    fn path_outside_home_stays_absolute() {
        assert_eq!(
            display_path_with_home(Path::new("/tmp/oven"), Path::new("/home/u")),
            "/tmp/oven"
        );
    }

    #[test]
    fn similar_prefix_is_not_treated_as_home() {
        assert_eq!(
            display_path_with_home(Path::new("/home/user2/x"), Path::new("/home/user")),
            "/home/user2/x"
        );
    }
}

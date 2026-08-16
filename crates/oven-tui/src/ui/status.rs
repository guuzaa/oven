use std::path::Path;
use std::time::{Duration, Instant};

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
const REPLY_TTL: Duration = Duration::from_secs(3);

/// Single status row below the input: model · root · token usage.
/// Optional slash-command reply is drawn on the row(s) beneath it.
pub struct StatusBar {
    model: String,
    effort: Option<ReasoningEffort>,
    root: String,
    total: Usage,
    reply: Option<String>,
    reply_until: Option<Instant>,
}

impl StatusBar {
    pub fn new(model: impl Into<String>, root: &Path, total: Usage) -> Self {
        Self {
            model: model.into(),
            effort: None,
            root: display_path(root),
            total,
            reply: None,
            reply_until: None,
        }
    }

    pub fn has_reply(&self) -> bool {
        self.reply.as_ref().is_some_and(|t| !t.is_empty())
    }

    pub fn expire_reply(&mut self) -> bool {
        match self.reply_until {
            Some(until) if Instant::now() >= until => {
                self.clear_reply();
                true
            }
            _ => false,
        }
    }

    pub fn reply_height(&self, width: u16) -> u16 {
        match self.reply.as_deref() {
            Some(text) if !text.is_empty() && width > 0 => {
                wrap_text(text, width as usize).len() as u16
            }
            _ => 0,
        }
    }

    pub fn draw_reply(&self, f: &mut Frame<'_>, area: Rect) {
        let Some(text) = self.reply.as_deref() else {
            return;
        };
        let lines: Vec<Line> = wrap_text(text, area.width as usize)
            .into_iter()
            .map(|s| Line::from(Span::styled(s, theme::reply())))
            .collect();
        f.render_widget(Paragraph::new(lines), area);
    }

    pub fn clear_reply(&mut self) {
        self.reply = None;
        self.reply_until = None;
    }

    fn set_reply(&mut self, text: String) {
        self.reply = Some(text);
        self.reply_until = Some(Instant::now() + REPLY_TTL);
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
                    self.clear_reply();
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
            AppEvent::ProviderUpdated { .. } => {}
            AppEvent::Error { .. } => {}
            AppEvent::Exit { .. } => {}
            AppEvent::Reply { text, .. } => {
                self.set_reply(text.clone());
            }
            AppEvent::Rewound { usage, .. } => {
                self.total = *usage;
                self.clear_reply();
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

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 || text.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for raw in text.lines() {
        if raw.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_w = 0;
        for ch in raw.chars() {
            let cw = ch.width().unwrap_or(0);
            if current_w + cw > width && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_w = 0;
            }
            current.push(ch);
            current_w += cw;
        }
        lines.push(current);
    }
    lines
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
    fn reply_event_sets_reply_and_height() {
        let mut bar = StatusBar::new("m", Path::new("/tmp"), Usage::default());
        let mut state = State::new();
        assert_eq!(bar.reply_height(80), 0);
        bar.on_event(
            &AppEvent::Reply {
                app_id: AppId(1),
                text: "current model: gpt-4o".into(),
            },
            &mut state,
        );
        assert_eq!(bar.reply.as_deref(), Some("current model: gpt-4o"));
        assert_eq!(bar.reply_height(80), 1);
        assert_eq!(bar.reply_height(8), 3);
        assert!(bar.has_reply());
        assert!(!bar.expire_reply());
    }

    #[test]
    fn reply_expires_once_ttl_elapses() {
        let mut bar = StatusBar::new("m", Path::new("/tmp"), Usage::default());
        bar.reply = Some("hi".into());
        bar.reply_until = Some(Instant::now() + Duration::from_secs(3));
        assert!(!bar.expire_reply());
        assert_eq!(bar.reply.as_deref(), Some("hi"));

        bar.reply_until = Some(Instant::now() - Duration::from_millis(1));
        assert!(bar.expire_reply());
        assert!(bar.reply.is_none());
        assert!(!bar.has_reply());
    }

    #[test]
    fn history_cleared_and_rewound_drop_reply() {
        let mut bar = StatusBar::new("m", Path::new("/tmp"), Usage::default());
        let mut state = State::new();
        bar.reply = Some("hi".into());
        bar.on_event(
            &agent_event(AgentEvent::HistoryCleared {
                agent_id: AgentId(1),
            }),
            &mut state,
        );
        assert!(bar.reply.is_none());

        bar.reply = Some("hi".into());
        bar.on_event(
            &AppEvent::Rewound {
                app_id: AppId(1),
                text: None,
                messages: Vec::new(),
                usage: Usage::default(),
            },
            &mut state,
        );
        assert!(bar.reply.is_none());
    }

    #[test]
    fn reply_paints_orange_on_the_row_below_status() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::layout::{Constraint, Direction, Layout};
        use ratatui::style::Color;

        let mut bar = StatusBar::new("m", Path::new("/tmp"), Usage::default());
        let mut state = State::new();
        bar.on_event(
            &AppEvent::Reply {
                app_id: AppId(1),
                text: "current model: gpt-4o".into(),
            },
            &mut state,
        );

        let backend = TestBackend::new(40, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(1), Constraint::Length(1)])
                    .split(f.area());
                bar.draw_bar(f, chunks[0], &state, false, 0);
                bar.draw_reply(f, chunks[1]);
            })
            .unwrap();

        let buf = terminal.backend().buffer();
        let row: String = (0..40).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(
            row.contains("current model: gpt-4o"),
            "reply row was {row:?}"
        );
        assert_eq!(buf[(0, 1)].style().fg, Some(Color::Rgb(255, 140, 0)));
    }

    #[test]
    fn wrap_text_splits_on_width_and_newlines() {
        assert!(wrap_text("", 10).is_empty());
        assert_eq!(wrap_text("abcd", 2), vec!["ab", "cd"]);
        assert_eq!(wrap_text("a\nb", 10), vec!["a", "b"]);
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

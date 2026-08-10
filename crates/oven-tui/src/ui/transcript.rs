use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use oven_agent::AgentEvent;
use oven_app::AppEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::component::{Component, KeyResult, State};
use super::tool_display;

const MOUSE_SCROLL_STEP: u16 = 3;
const START_HINT: &str =
    "oven — Enter send · Alt-Enter newline · PgUp/PgDn scroll · Esc cancel · Ctrl-C quit";

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    User,
    Thinking,
    Text,
    Tool,
    Error,
    System,
}

pub struct Transcript {
    line_cache: Vec<Line<'static>>,
    streaming: String,
    stream_kind: LineKind,
    scroll: u16,
    area: Rect,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            line_cache: format_lines(LineKind::System, START_HINT),
            streaming: String::new(),
            stream_kind: LineKind::Text,
            scroll: 0,
            area: Rect::default(),
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.push_row(LineKind::User, text);
    }

    fn reset(&mut self) {
        self.line_cache = format_lines(LineKind::System, START_HINT);
        self.streaming.clear();
        self.stream_kind = LineKind::Text;
        self.scroll = 0;
    }

    fn push_row(&mut self, kind: LineKind, text: &str) {
        self.line_cache.extend(format_lines(kind, text));
        self.scroll = 0;
    }

    fn flush_streaming(&mut self) {
        let body = std::mem::take(&mut self.streaming);
        let body = trim_message(&body);
        if !body.is_empty() {
            self.push_row(self.stream_kind, &body);
        }
    }

    fn push_stream(&mut self, kind: LineKind, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.streaming.is_empty() && self.stream_kind != kind {
            self.flush_streaming();
        }
        self.stream_kind = kind;
        self.streaming.push_str(text);
        self.scroll = 0;
    }
}

impl Component for Transcript {
    fn handle_key(&mut self, key: KeyEvent, _state: &State) -> KeyResult {
        match key.code {
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_add(1);
                KeyResult::Handled
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(1);
                KeyResult::Handled
            }
            _ => KeyResult::Ignored,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, _state: &State) -> KeyResult {
        if !self.area.contains(ratatui::layout::Position {
            x: mouse.column,
            y: mouse.row,
        }) {
            return KeyResult::Ignored;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.scroll = self.scroll.saturating_add(MOUSE_SCROLL_STEP);
                KeyResult::Handled
            }
            MouseEventKind::ScrollDown => {
                self.scroll = self.scroll.saturating_sub(MOUSE_SCROLL_STEP);
                KeyResult::Handled
            }
            _ => KeyResult::Ignored,
        }
    }

    fn on_event(&mut self, ev: &AppEvent, _state: &mut State) {
        match ev {
            AppEvent::Agent { event, .. } => match event {
                AgentEvent::ThinkingDelta { text, .. } => {
                    self.push_stream(LineKind::Thinking, text);
                }
                AgentEvent::TextDelta { text, .. } => {
                    self.push_stream(LineKind::Text, text);
                }
                AgentEvent::ToolStart { name, input, .. } => {
                    self.flush_streaming();
                    self.push_row(LineKind::Tool, &tool_display(name, input));
                }
                AgentEvent::ToolEnd { .. } => {}
                AgentEvent::Done { text, .. } => {
                    if self.stream_kind == LineKind::Text {
                        self.streaming.clear();
                    } else {
                        self.flush_streaming();
                    }
                    let body = trim_message(text);
                    if !body.is_empty() {
                        self.push_row(LineKind::Text, &body);
                    }
                }
                AgentEvent::Cancelled { .. } => {
                    if !self.streaming.is_empty() {
                        let kind = self.stream_kind;
                        let partial = trim_message(&std::mem::take(&mut self.streaming));
                        if !partial.is_empty() {
                            self.push_row(kind, &format!("{partial}…"));
                        }
                    }
                    self.push_row(LineKind::System, "cancelled");
                }
                AgentEvent::Exit { .. } => {}
                AgentEvent::HistoryCleared { .. } => self.reset(),
            },
            AppEvent::Idle { .. } => {
                self.flush_streaming();
            }
            AppEvent::Error { message, .. } => {
                self.flush_streaming();
                self.push_row(LineKind::Error, message);
            }
        }
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect, _state: &State) {
        self.area = area;
        let width = area.width.saturating_sub(2) as usize;
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(self.line_cache.len() + 8);
        for line in &self.line_cache {
            wrap_line_into(&mut lines, line, width);
        }
        if !self.streaming.is_empty() {
            for line in format_lines(self.stream_kind, &self.streaming) {
                wrap_line_into(&mut lines, &line, width);
            }
        }

        let height = area.height.saturating_sub(2) as usize;
        let total = lines.len();
        let max_scroll = total.saturating_sub(height);
        let scroll = (self.scroll as usize).min(max_scroll);
        let start = total.saturating_sub(height + scroll);
        let end = total.saturating_sub(scroll);
        let visible = lines[start..end].to_vec();

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" conversation ");
        let para = Paragraph::new(visible).block(block);
        f.render_widget(para, area);
    }
}

fn trim_message(text: &str) -> String {
    text.trim_matches(|c: char| c == '\n' || c == '\r')
        .to_string()
}

const LINE_PREFIX_WIDTH: usize = 11;
const LINE_INDENT: &str = "           ";

fn format_lines(kind: LineKind, text: &str) -> Vec<Line<'static>> {
    let (prefix, style) = match kind {
        LineKind::User => (
            "you      | ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        LineKind::Thinking => ("thinking | ", Style::default().fg(Color::DarkGray)),
        LineKind::Text => ("text     | ", Style::default().fg(Color::Green)),
        LineKind::Tool => ("tool     | ", Style::default().fg(Color::Magenta)),
        LineKind::Error => ("err      | ", Style::default().fg(Color::Red)),
        LineKind::System => ("sys      | ", Style::default().fg(Color::DarkGray)),
    };
    let mut lines = Vec::new();
    let mut prev_blank = false;
    for part in text.lines() {
        let blank = part.trim().is_empty();
        if blank && (prev_blank || lines.is_empty()) {
            continue;
        }
        prev_blank = blank;
        let head = if lines.is_empty() {
            prefix
        } else {
            LINE_INDENT
        };
        lines.push(Line::from(vec![
            Span::styled(head.to_string(), style),
            Span::raw(if blank {
                String::new()
            } else {
                part.to_string()
            }),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), style),
            Span::raw(String::new()),
        ]));
    }
    lines
}

fn wrap_line_into(out: &mut Vec<Line<'static>>, line: &Line<'static>, width: usize) {
    if width == 0 {
        out.push(line.clone());
        return;
    }
    let (prefix, style, body) = match line.spans.as_slice() {
        [head, rest @ ..] => {
            let body: String = rest.iter().map(|s| s.content.as_ref()).collect();
            (head.content.as_ref().to_string(), head.style, body)
        }
        [] => {
            out.push(line.clone());
            return;
        }
    };
    let body_width = width.saturating_sub(LINE_PREFIX_WIDTH).max(1);
    if body.is_empty() {
        out.push(Line::from(vec![
            Span::styled(prefix, style),
            Span::raw(String::new()),
        ]));
        return;
    }
    let mut first = true;
    let mut rest = body.as_str();
    while !rest.is_empty() {
        let (chunk, next) = split_at_width(rest, body_width);
        let head = if first {
            first = false;
            prefix.as_str()
        } else {
            LINE_INDENT
        };
        out.push(Line::from(vec![
            Span::styled(head.to_string(), style),
            Span::raw(chunk.to_string()),
        ]));
        rest = next;
    }
}

fn split_at_width(s: &str, max_width: usize) -> (&str, &str) {
    if max_width == 0 {
        return ("", s);
    }
    if s.width() <= max_width {
        return (s, "");
    }
    let mut width = 0;
    for (i, ch) in s.char_indices() {
        let cw = ch.width().unwrap_or(0);
        if width + cw > max_width {
            if i == 0 {
                let next = ch.len_utf8();
                return (&s[..next], &s[next..]);
            }
            return (&s[..i], &s[i..]);
        }
        width += cw;
    }
    (s, "")
}

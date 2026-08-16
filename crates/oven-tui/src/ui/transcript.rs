use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use oven_app::{AgentEvent, AppEvent};
use oven_llm::{ContentBlock, Message, Role};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::component::{Component, KeyResult, State};
use super::theme;
use super::tool_display;

const MOUSE_SCROLL_STEP: u16 = 3;
const LINE_PREFIX_WIDTH: usize = 2;
const LINE_INDENT: &str = "  ";
const MAX_RESULT_LINES: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineKind {
    User,
    Thinking,
    Text,
    Tool,
    ToolResult(bool),
    Error,
    System,
}

impl LineKind {
    fn style(self) -> Style {
        match self {
            LineKind::User => theme::user(),
            LineKind::Thinking => theme::thinking(),
            LineKind::Text => theme::assistant(),
            LineKind::Tool => theme::tool(),
            LineKind::ToolResult(true) => theme::ok(),
            LineKind::ToolResult(false) => theme::fail(),
            LineKind::Error => theme::error(),
            LineKind::System => theme::dim(),
        }
    }

    fn gutter(self) -> &'static str {
        match self {
            LineKind::User => "› ",
            LineKind::Text => "• ",
            LineKind::Thinking => "· ",
            LineKind::Tool => "$ ",
            LineKind::ToolResult(_) | LineKind::Error | LineKind::System => "  ",
        }
    }
}

/// One source row of the conversation. The wrapped rendering is cached in
/// `Transcript::wrapped` so redraws only re-wrap the live stream.
struct Row {
    kind: LineKind,
    text: String,
}

pub struct Transcript {
    rows: Vec<Row>,
    wrapped: Vec<Line<'static>>,
    streaming: String,
    stream_kind: LineKind,
    wrapped_stream: Vec<Line<'static>>,
    stream_dirty: bool,
    /// User messages queued while busy; rendered as `User` rows once the
    /// in-flight answer finishes so they appear after it.
    pending_user: Vec<String>,
    /// Content width (excluding borders) used by the cached wrap; 0 until
    /// the first draw.
    width: usize,
    /// Viewport: when `pinned` the view follows the newest content; when the
    /// user scrolled up, `top` is the absolute wrapped-line index of the
    /// first visible line and stays put while new content arrives.
    pinned: bool,
    top: usize,
    /// Viewport height from the last draw, used by scroll commands.
    view_height: usize,
    area: Rect,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            wrapped: Vec::new(),
            streaming: String::new(),
            stream_kind: LineKind::Text,
            wrapped_stream: Vec::new(),
            stream_dirty: false,
            pending_user: Vec::new(),
            width: 0,
            pinned: true,
            top: 0,
            view_height: 0,
            area: Rect::default(),
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.push_row(LineKind::User, text);
    }

    /// Remember a user message accepted while the app was busy; it is shown
    /// as a normal user row once the current answer finishes.
    pub fn push_user_queued(&mut self, text: &str) {
        self.pending_user.push(text.to_string());
    }

    /// Text of the most recent user message, or `None` when the transcript
    /// has none (fresh session or everything already rewound).
    pub(crate) fn last_user_text(&self) -> Option<String> {
        self.rows
            .iter()
            .rev()
            .find(|r| r.kind == LineKind::User)
            .map(|r| r.text.clone())
    }

    /// Drop the most recently queued user message; mirrors the input queue
    /// so popped messages do not reappear once the turn ends.
    pub(crate) fn pop_pending_user(&mut self) -> Option<String> {
        self.pending_user.pop()
    }

    /// Rebuild the transcript from a message list (e.g. after a rewind).
    /// Keeps queued user messages and returns the view to following the
    /// newest content.
    pub(crate) fn replace_from(&mut self, messages: &[Message]) {
        self.rows.clear();
        self.wrapped.clear();
        self.clear_stream();
        self.pinned = true;
        self.top = 0;
        self.seed(messages);
    }

    /// Pre-fill the transcript from a persisted session's messages when
    /// resuming. Renders the same row kinds the live event stream produces;
    /// images are skipped since they cannot be drawn in a terminal.
    pub fn seed(&mut self, messages: &[Message]) {
        for m in messages {
            match m.role {
                Role::User => {
                    for block in &m.content {
                        match block {
                            ContentBlock::Text { text } => {
                                self.push_row(LineKind::User, text);
                            }
                            ContentBlock::ToolResult {
                                content, is_error, ..
                            } => self.push_tool_result(*is_error, content),
                            _ => {}
                        }
                    }
                }
                Role::Tool => {
                    for block in &m.content {
                        if let ContentBlock::ToolResult {
                            content, is_error, ..
                        } = block
                        {
                            self.push_tool_result(*is_error, content);
                        }
                    }
                }
                Role::Assistant => {
                    for block in &m.content {
                        match block {
                            ContentBlock::Thinking { thinking } => {
                                self.push_row(LineKind::Thinking, thinking);
                            }
                            ContentBlock::Text { text } => {
                                let body = trim_message(text);
                                if !body.is_empty() {
                                    self.push_row(LineKind::Text, &body);
                                }
                            }
                            ContentBlock::ToolUse { name, input, .. } => {
                                self.push_row(LineKind::Tool, &tool_display(name, input));
                            }
                            _ => {}
                        }
                    }
                }
                Role::System => {}
            }
        }
    }

    fn push_tool_result(&mut self, is_error: bool, content: &[ContentBlock]) {
        let tool_result = content
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Text { text } = block {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<&str>>()
            .join("\n");
        let body = trim_message(&tool_result);
        if is_error || !body.is_empty() {
            self.push_row(
                LineKind::ToolResult(is_error),
                if body.is_empty() {
                    "(no output)"
                } else {
                    body.as_str()
                },
            );
        }
    }

    fn total_lines(&self) -> usize {
        self.wrapped.len() + self.wrapped_stream.len()
    }

    fn current_top(&self) -> usize {
        if self.pinned {
            self.total_lines().saturating_sub(self.view_height)
        } else {
            self.top
        }
    }

    /// Keep the viewport anchored: a pinned view follows the newest content,
    /// a scrolled-up view keeps its absolute position.
    fn keep_following(&mut self) {
        if self.pinned {
            self.top = self.total_lines().saturating_sub(self.view_height);
        }
    }

    fn scroll_up(&mut self, n: u16) {
        self.top = self.current_top().saturating_sub(n as usize);
        self.pinned = false;
    }

    fn scroll_down(&mut self, n: u16) {
        let total = self.total_lines();
        let height = self.view_height.max(1);
        let max_top = total.saturating_sub(height);
        let top = self.current_top().saturating_add(n as usize).min(max_top);
        self.top = top;
        self.pinned = top.saturating_add(height) >= total;
    }

    fn reset(&mut self) {
        self.rows.clear();
        self.wrapped.clear();
        self.clear_stream();
        self.pending_user.clear();
        self.pinned = true;
        self.top = 0;
    }

    fn push_row(&mut self, kind: LineKind, text: &str) {
        let text = match kind {
            LineKind::Thinking => collapse_thinking(text),
            LineKind::ToolResult(_) => truncate_result(text),
            _ => text.to_string(),
        };
        let mut wrapped = Vec::new();
        if self.width > 0 {
            if !self.wrapped.is_empty() {
                wrapped.push(Line::from(""));
            }
            for line in format_lines(kind, &text) {
                wrap_line_into(&mut wrapped, &line, self.width);
            }
        }
        self.rows.push(Row { kind, text });
        self.wrapped.extend(wrapped);
        self.keep_following();
    }

    fn flush_streaming(&mut self) {
        let body = trim_message(&std::mem::take(&mut self.streaming));
        self.wrapped_stream.clear();
        self.stream_dirty = false;
        if !body.is_empty() {
            self.push_row(self.stream_kind, &body);
        }
    }

    /// Place queued user messages at the end of the transcript, after the
    /// current activity (tool result or final answer) is rendered.
    fn flush_pending_user(&mut self) {
        std::mem::take(&mut self.pending_user)
            .into_iter()
            .for_each(|text| self.push_row(LineKind::User, &text));
    }

    fn clear_stream(&mut self) {
        self.streaming.clear();
        self.wrapped_stream.clear();
        self.stream_dirty = false;
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
        self.stream_dirty = true;
        self.keep_following();
    }

    fn wrap_rows_from(&mut self, start: usize) {
        let width = self.width;
        if width == 0 {
            return;
        }
        for row in &self.rows[start..] {
            if !self.wrapped.is_empty() {
                self.wrapped.push(Line::from(""));
            }
            for line in format_lines(row.kind, &row.text) {
                wrap_line_into(&mut self.wrapped, &line, width);
            }
        }
    }

    fn rewrap_stream(&mut self) {
        self.wrapped_stream.clear();
        self.stream_dirty = false;
        if self.width == 0 || self.streaming.is_empty() {
            return;
        }
        if !self.wrapped.is_empty() {
            self.wrapped_stream.push(Line::from(""));
        }
        for line in format_lines(self.stream_kind, &self.streaming) {
            wrap_line_into(&mut self.wrapped_stream, &line, self.width);
        }
    }

    fn rewrap_all(&mut self) {
        self.wrapped.clear();
        self.wrap_rows_from(0);
        self.rewrap_stream();
    }
}

impl Component for Transcript {
    fn handle_key(&mut self, key: KeyEvent, _state: &State) -> KeyResult {
        match key.code {
            KeyCode::PageUp => {
                self.scroll_up(1);
                KeyResult::Handled
            }
            KeyCode::PageDown => {
                self.scroll_down(1);
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
                self.scroll_up(MOUSE_SCROLL_STEP);
                KeyResult::Handled
            }
            MouseEventKind::ScrollDown => {
                self.scroll_down(MOUSE_SCROLL_STEP);
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
                AgentEvent::ToolEnd { ok, output, .. } => {
                    let body = trim_message(output);
                    if !*ok || !body.is_empty() {
                        let body = if body.is_empty() {
                            "(no output)".to_string()
                        } else {
                            body
                        };
                        self.push_row(LineKind::ToolResult(*ok), &body);
                    }
                    self.flush_pending_user();
                }
                AgentEvent::Done { text, .. } => {
                    if self.stream_kind == LineKind::Text {
                        self.clear_stream();
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
                        self.wrapped_stream.clear();
                        self.stream_dirty = false;
                        if !partial.is_empty() {
                            self.push_row(kind, &format!("{partial}…"));
                        }
                    }
                    self.push_row(LineKind::System, "cancelled");
                }
                AgentEvent::HistoryCleared { .. } => self.reset(),
                AgentEvent::ModelChanged { .. } => {}
            },
            AppEvent::ModelsUpdated { .. } => {}
            AppEvent::ProviderUpdated { .. } => {}
            AppEvent::Exit { .. } => {}
            AppEvent::Rewound { messages, .. } => self.replace_from(messages),
            AppEvent::Idle { .. } => {
                self.flush_streaming();
                self.flush_pending_user();
            }
            AppEvent::Error { message, .. } => {
                self.flush_streaming();
                self.push_row(LineKind::Error, message);
            }
        }
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect, _state: &State) {
        self.area = area;
        let width = area.width as usize;
        if width != self.width {
            self.width = width;
            self.rewrap_all();
        } else if self.stream_dirty {
            self.rewrap_stream();
        }

        let height = area.height as usize;
        self.view_height = height;
        let total = self.total_lines();
        let max_top = total.saturating_sub(height);
        if !self.pinned {
            self.top = self.top.min(max_top);
            if self.top.saturating_add(height) >= total {
                self.pinned = true;
            }
        }
        let start = if self.pinned { max_top } else { self.top };
        let end = start.saturating_add(height).min(total);
        let visible = collect_lines(&self.wrapped, &self.wrapped_stream, start, end);
        f.render_widget(Paragraph::new(visible), area);
    }
}

fn collect_lines(
    history: &[Line<'static>],
    stream: &[Line<'static>],
    start: usize,
    end: usize,
) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(end.saturating_sub(start));
    for idx in start..end {
        if idx < history.len() {
            out.push(history[idx].clone());
        } else {
            out.push(stream[idx - history.len()].clone());
        }
    }
    out
}

fn trim_message(text: &str) -> String {
    text.trim_matches(|c: char| c == '\n' || c == '\r')
        .to_string()
}

fn collapse_thinking(text: &str) -> String {
    let text = trim_message(text);
    let mut iter = text.lines();
    let first = iter.next().unwrap_or("").trim_end();
    if iter.next().is_some() {
        format!("{first}…")
    } else {
        first.to_string()
    }
}

fn truncate_result(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= MAX_RESULT_LINES {
        return text.to_string();
    }
    let mut out = lines[..MAX_RESULT_LINES].join("\n");
    out.push_str(&format!("\n… {} more", lines.len() - MAX_RESULT_LINES));
    out
}

fn format_lines(kind: LineKind, text: &str) -> Vec<Line<'static>> {
    let prefix = kind.gutter();
    let style = kind.style();
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

#[cfg(test)]
mod tests {
    use super::*;
    use oven_app::{AgentId, AppId};

    fn wide(t: &mut Transcript) {
        t.width = 80;
        t.rewrap_all();
    }

    fn fill(t: &mut Transcript, n: usize) {
        for i in 0..n {
            t.push_row(LineKind::Text, &format!("line {i}"));
        }
    }

    #[test]
    fn collapse_thinking_keeps_first_line() {
        assert_eq!(collapse_thinking("one\ntwo\nthree"), "one…");
        assert_eq!(collapse_thinking("only"), "only");
    }

    #[test]
    fn truncate_result_caps_lines() {
        let text = (0..10)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = truncate_result(&text);
        assert!(out.ends_with("… 4 more"));
        assert_eq!(out.lines().count(), MAX_RESULT_LINES + 1);
    }

    #[test]
    fn all_gutters_are_two_wide() {
        let kinds = [
            LineKind::User,
            LineKind::Thinking,
            LineKind::Text,
            LineKind::Tool,
            LineKind::ToolResult(true),
            LineKind::ToolResult(false),
            LineKind::Error,
            LineKind::System,
        ];
        for kind in kinds {
            assert_eq!(kind.gutter().width(), LINE_PREFIX_WIDTH);
        }
    }

    #[test]
    fn assistant_gutter_is_bullet() {
        let lines = format_lines(LineKind::Text, "hi");
        assert_eq!(lines[0].spans[0].content.as_ref(), "• ");
    }

    #[test]
    fn tool_result_gutters() {
        let ok = format_lines(LineKind::ToolResult(true), "out");
        assert_eq!(ok[0].spans[0].content.as_ref(), "  ");
        let fail = format_lines(LineKind::ToolResult(false), "boom");
        assert_eq!(fail[0].spans[0].content.as_ref(), "  ");
    }

    #[test]
    fn streaming_does_not_yank_scrolled_view() {
        let mut t = Transcript::new();
        wide(&mut t);
        t.view_height = 3;
        fill(&mut t, 10);
        t.scroll_up(2);
        let anchored = t.top;
        assert!(!t.pinned);
        t.push_stream(LineKind::Text, "more");
        assert_eq!(t.top, anchored, "reading position must not move");
        assert!(!t.pinned);
    }

    #[test]
    fn streaming_follows_when_pinned() {
        let mut t = Transcript::new();
        wide(&mut t);
        t.view_height = 3;
        fill(&mut t, 10);
        assert!(t.pinned);
        t.push_stream(LineKind::Text, "more");
        assert!(t.pinned);
        t.rewrap_stream();
        assert_eq!(
            t.current_top(),
            t.total_lines().saturating_sub(t.view_height)
        );
    }

    #[test]
    fn scroll_down_returns_to_bottom() {
        let mut t = Transcript::new();
        wide(&mut t);
        t.view_height = 3;
        fill(&mut t, 10);
        t.scroll_up(5);
        assert!(!t.pinned);
        t.scroll_down(5);
        assert!(t.pinned);
    }

    #[test]
    fn tool_end_renders_result_row() {
        let mut t = Transcript::new();
        let ev = AppEvent::Agent {
            app_id: AppId(1),
            event: AgentEvent::ToolEnd {
                agent_id: AgentId(1),
                call_id: "c1".into(),
                ok: false,
                output: "boom\n".into(),
            },
        };
        t.on_event(&ev, &mut State::new());
        let row = t.rows.last().unwrap();
        assert_eq!(row.kind, LineKind::ToolResult(false));
        assert_eq!(row.text, "boom");
    }

    #[test]
    fn empty_ok_tool_end_renders_nothing() {
        let mut t = Transcript::new();
        let n = t.rows.len();
        let ev = AppEvent::Agent {
            app_id: AppId(1),
            event: AgentEvent::ToolEnd {
                agent_id: AgentId(1),
                call_id: "c1".into(),
                ok: true,
                output: String::new(),
            },
        };
        t.on_event(&ev, &mut State::new());
        assert_eq!(t.rows.len(), n);
    }

    #[test]
    fn push_user_queued_is_deferred_until_idle() {
        let mut t = Transcript::new();
        let n = t.rows.len();
        t.push_user_queued("hello");
        assert_eq!(t.rows.len(), n);
        t.on_event(&AppEvent::Idle { app_id: AppId(1) }, &mut State::new());
        assert_eq!(
            t.rows.last().map(|r| (r.kind, r.text.as_str())),
            Some((LineKind::User, "hello"))
        );
    }

    #[test]
    fn queued_user_renders_after_streamed_answer() {
        let mut t = Transcript::new();
        t.push_stream(LineKind::Text, "answer");
        t.push_user_queued("hello");
        t.on_event(&AppEvent::Idle { app_id: AppId(1) }, &mut State::new());
        let kinds: Vec<LineKind> = t.rows.iter().map(|r| r.kind).collect();
        assert_eq!(&kinds[kinds.len() - 2..], &[LineKind::Text, LineKind::User]);
    }

    #[test]
    fn queued_user_renders_after_tool_end() {
        let mut t = Transcript::new();
        t.push_user_queued("hello");
        let ev = AppEvent::Agent {
            app_id: AppId(1),
            event: AgentEvent::ToolEnd {
                agent_id: AgentId(1),
                call_id: "c1".into(),
                ok: false,
                output: "boom\n".into(),
            },
        };
        t.on_event(&ev, &mut State::new());
        let kinds: Vec<LineKind> = t.rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            &kinds[kinds.len() - 2..],
            &[LineKind::ToolResult(false), LineKind::User]
        );
        assert_eq!(t.rows.last().map(|r| r.text.as_str()), Some("hello"));
    }

    #[test]
    fn queued_user_renders_after_empty_tool_end() {
        let mut t = Transcript::new();
        t.push_user_queued("hello");
        let ev = AppEvent::Agent {
            app_id: AppId(1),
            event: AgentEvent::ToolEnd {
                agent_id: AgentId(1),
                call_id: "c1".into(),
                ok: true,
                output: String::new(),
            },
        };
        t.on_event(&ev, &mut State::new());
        assert_eq!(
            t.rows.last().map(|r| (r.kind, r.text.as_str())),
            Some((LineKind::User, "hello"))
        );
    }

    #[test]
    fn seed_renders_persisted_messages() {
        let mut t = Transcript::new();
        let messages = vec![
            Message::user_text("hello"),
            Message::assistant(vec![
                ContentBlock::Thinking {
                    thinking: "hmm".into(),
                },
                ContentBlock::Text {
                    text: "hi there".into(),
                },
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({ "command": "ls" }),
                },
            ]),
            Message::tool_result("c1", "done", false),
            Message::user(vec![
                ContentBlock::Text {
                    text: "thanks".into(),
                },
                ContentBlock::ToolResult {
                    tool_use_id: "c1".into(),
                    content: vec![ContentBlock::text("boom")],
                    is_error: true,
                },
            ]),
        ];
        t.seed(&messages);

        let kinds: Vec<LineKind> = t.rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                LineKind::User,
                LineKind::Thinking,
                LineKind::Text,
                LineKind::Tool,
                LineKind::ToolResult(false),
                LineKind::User,
                LineKind::ToolResult(true),
            ]
        );
        assert_eq!(t.rows[0].text, "hello");
        assert_eq!(t.rows[3].text, "ls");
    }

    #[test]
    fn seed_empty_history_is_empty() {
        let mut t = Transcript::new();
        t.seed(&[]);
        assert!(t.rows.is_empty());
    }

    #[test]
    fn seed_mirrors_empty_tool_result_handling() {
        let mut t = Transcript::new();
        t.seed(&[Message::tool_result("c1", "", false)]);
        assert!(t.rows.is_empty(), "empty ok output is skipped");

        let mut t = Transcript::new();
        t.seed(&[Message::tool_result("c1", "", true)]);
        assert_eq!(
            t.rows.last().map(|r| (r.kind, r.text.as_str())),
            Some((LineKind::ToolResult(true), "(no output)"))
        );
    }

    #[test]
    fn last_user_text_returns_most_recent_user_row() {
        let mut t = Transcript::new();
        t.push_user("first");
        t.push_row(LineKind::Text, "one");
        t.push_user("second");
        assert_eq!(t.last_user_text().as_deref(), Some("second"));
    }

    #[test]
    fn last_user_text_none_without_user_rows() {
        let mut t = Transcript::new();
        t.push_row(LineKind::Text, "one");
        assert_eq!(t.last_user_text(), None);
    }

    #[test]
    fn pop_pending_user_pops_last() {
        let mut t = Transcript::new();
        t.push_user_queued("a");
        t.push_user_queued("b");
        assert_eq!(t.pop_pending_user().as_deref(), Some("b"));
        assert_eq!(t.pop_pending_user().as_deref(), Some("a"));
        assert_eq!(t.pop_pending_user(), None);
    }

    #[test]
    fn replace_from_rebuilds_rows_and_keeps_pending_user() {
        let mut t = Transcript::new();
        t.push_user("old");
        t.push_user_queued("queued");
        t.replace_from(&[Message::user_text("resumed")]);
        assert_eq!(t.rows[0].text, "resumed");
        assert_eq!(t.pending_user, vec!["queued".to_string()]);
    }
}

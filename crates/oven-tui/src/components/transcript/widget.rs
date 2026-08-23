use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use oven_app::{
    AgentEvent, AppEvent, AppEventKind, StreamEvent, ToolEvent, ToolResult, ToolView, TurnEvent,
    present_tool,
};
use oven_llm::{ContentBlock, Message, Role};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::super::component::{Action, Component, KeyResult, State};
use super::super::theme;
use super::kinds::{LineKind, Row, THINKING_LABEL, THOUGHT_LABEL};
use super::selection::{SelPos, copy_to_clipboard, extract_line_range, highlight_line};
use super::tools::{ToolBurst, compact_tool_arg, format_tool_summary};
use super::wrap::{
    apply_thinking_shimmer, collect_lines, format_lines, line_display_width, paint_visible,
    thinking_phase, trim_message, truncate_result, wrap_line_into, wrap_row_into,
};

const MOUSE_SCROLL_STEP: u16 = 3;
const STREAM_CARET: &str = "▊";
const CARET_FRAMES: u64 = 5;

pub struct Transcript {
    pub(super) rows: Vec<Row>,
    pub(super) wrapped: Vec<Line<'static>>,
    streaming: String,
    stream_kind: LineKind,
    wrapped_stream: Vec<Line<'static>>,
    stream_dirty: bool,
    /// Content width (excluding borders) used by the cached wrap; 0 until
    /// the first draw.
    pub(super) width: usize,
    /// Viewport: when `pinned` the view follows the newest content; when the
    /// user scrolled up, `top` is the absolute wrapped-line index of the
    /// first visible line and stays put while new content arrives.
    pub(super) pinned: bool,
    pub(super) top: usize,
    /// Viewport height from the last draw, used by scroll commands.
    pub(super) view_height: usize,
    pub(super) area: Rect,
    pub(super) select_anchor: Option<SelPos>,
    select_head: Option<SelPos>,
    pub(super) dragging: bool,
    tool_burst: ToolBurst,
    detail_ids: HashSet<String>,
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
            width: 0,
            pinned: true,
            top: 0,
            view_height: 0,
            area: Rect::default(),
            select_anchor: None,
            select_head: None,
            dragging: false,
            tool_burst: ToolBurst::default(),
            detail_ids: HashSet::new(),
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.close_tool_burst();
        self.push_row(LineKind::User, text);
    }

    pub(crate) fn last_user_text(&self) -> Option<String> {
        self.rows
            .iter()
            .rev()
            .find(|r| r.kind == LineKind::User)
            .map(|r| r.text.clone())
    }

    pub(crate) fn replace_from(&mut self, messages: &[Message]) {
        self.reset();
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
                                self.close_tool_burst();
                                self.push_row(LineKind::User, text);
                            }
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } => self.note_seed_result(tool_use_id, *is_error, content),
                            _ => {}
                        }
                    }
                }
                Role::Tool => {
                    for block in &m.content {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } = block
                        {
                            self.note_seed_result(tool_use_id, *is_error, content);
                        }
                    }
                }
                Role::Assistant => {
                    let mut emitted = false;
                    let mut has_tool = false;
                    for block in &m.content {
                        match block {
                            ContentBlock::Thinking { .. } => {
                                self.close_tool_burst();
                                if !matches!(
                                    self.rows.last().map(|r| r.kind),
                                    Some(LineKind::Thinking)
                                ) {
                                    self.push_row(LineKind::Thinking, THOUGHT_LABEL);
                                }
                                emitted = true;
                            }
                            ContentBlock::Text { text } => {
                                let body = trim_message(text);
                                if !body.is_empty() {
                                    self.close_tool_burst();
                                    self.push_row(LineKind::Text, &body);
                                    emitted = true;
                                }
                            }
                            ContentBlock::ToolUse { id, name, input } => {
                                self.note_tool_start(id, &present_tool(name, input));
                                emitted = true;
                                has_tool = true;
                            }
                            _ => {}
                        }
                    }
                    if emitted && !has_tool {
                        self.push_separator();
                    }
                }
                Role::System => {}
            }
        }
        self.close_tool_burst();
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

    pub(super) fn total_lines(&self) -> usize {
        self.wrapped.len() + self.wrapped_stream.len()
    }

    pub(super) fn current_top(&self) -> usize {
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

    pub(super) fn scroll_up(&mut self, n: u16) {
        self.top = self.current_top().saturating_sub(n as usize);
        self.pinned = false;
    }

    pub(super) fn scroll_down(&mut self, n: u16) {
        let total = self.total_lines();
        let height = self.view_height.max(1);
        let max_top = total.saturating_sub(height);
        let top = self.current_top().saturating_add(n as usize).min(max_top);
        self.top = top;
        self.pinned = top.saturating_add(height) >= total;
    }

    pub(super) fn reset(&mut self) {
        self.rows.clear();
        self.wrapped.clear();
        self.clear_stream();
        self.pinned = true;
        self.top = 0;
        self.clear_selection();
        self.close_tool_burst();
        self.detail_ids.clear();
    }

    fn close_tool_burst(&mut self) {
        self.tool_burst = ToolBurst::default();
    }

    fn note_tool_start(&mut self, call_id: &str, view: &ToolView) {
        if !view.collapse {
            self.close_tool_burst();
            self.detail_ids.insert(call_id.to_string());
            self.push_row(LineKind::Tool, &view.summary);
            return;
        }
        let label = compact_tool_arg(&view.summary);
        self.tool_burst
            .pending
            .insert(call_id.to_string(), label.clone());
        self.tool_burst.bump(&label);
        self.upsert_tool_summary();
    }

    fn note_tool_end(&mut self, call_id: &str, ok: bool, output: &str) {
        if self.detail_ids.remove(call_id) {
            let body = trim_message(output);
            if !ok || !body.is_empty() {
                let body = if body.is_empty() {
                    "(no output)".to_string()
                } else {
                    body
                };
                self.push_row(LineKind::ToolResult(ok), &body);
            }
            return;
        }
        let Some(name) = self.tool_burst.pending.remove(call_id) else {
            return;
        };
        if !ok {
            self.tool_burst.bump_failed(&name);
            self.upsert_tool_summary();
        }
    }

    fn note_seed_result(&mut self, tool_use_id: &str, is_error: bool, content: &[ContentBlock]) {
        if self.detail_ids.remove(tool_use_id) {
            self.push_tool_result(is_error, content);
            return;
        }
        let Some(name) = self.tool_burst.pending.remove(tool_use_id) else {
            return;
        };
        if is_error {
            self.tool_burst.bump_failed(&name);
            self.upsert_tool_summary();
        }
    }

    fn upsert_tool_summary(&mut self) {
        let text = format_tool_summary(&self.tool_burst.entries);
        if self.tool_burst.row_open {
            self.replace_last_row(LineKind::Tool, &text);
        } else {
            self.tool_burst.wrap_at = self.wrapped.len();
            self.push_row(LineKind::Tool, &text);
            self.tool_burst.row_open = true;
        }
    }

    fn replace_last_row(&mut self, kind: LineKind, text: &str) {
        let text = match kind {
            LineKind::Thinking => THOUGHT_LABEL.to_string(),
            LineKind::ToolResult(_) => truncate_result(text),
            LineKind::Separator => String::new(),
            _ => text.to_string(),
        };
        if let Some(last) = self.rows.last_mut() {
            last.kind = kind;
            last.text = text.clone();
        }
        self.wrapped.truncate(self.tool_burst.wrap_at);
        if self.width > 0 {
            wrap_row_into(&mut self.wrapped, kind, &text, self.width);
        }
        self.keep_following();
    }

    pub(super) fn push_row(&mut self, kind: LineKind, text: &str) {
        let text = match kind {
            LineKind::Thinking => THOUGHT_LABEL.to_string(),
            LineKind::ToolResult(_) => truncate_result(text),
            LineKind::Separator => String::new(),
            _ => text.to_string(),
        };
        let mut wrapped = Vec::new();
        if self.width > 0 {
            wrap_row_into(&mut wrapped, kind, &text, self.width);
        }
        self.rows.push(Row { kind, text });
        self.wrapped.extend(wrapped);
        self.keep_following();
    }

    fn push_separator(&mut self) {
        if matches!(
            self.rows.last().map(|r| r.kind),
            Some(LineKind::Separator) | None
        ) {
            return;
        }
        self.push_row(LineKind::Separator, "");
    }

    fn flush_streaming(&mut self) {
        let body = trim_message(&std::mem::take(&mut self.streaming));
        self.wrapped_stream.clear();
        self.stream_dirty = false;
        if !body.is_empty() {
            self.push_row(self.stream_kind, &body);
        }
    }

    fn clear_stream(&mut self) {
        self.streaming.clear();
        self.wrapped_stream.clear();
        self.stream_dirty = false;
    }

    pub(super) fn push_stream(&mut self, kind: LineKind, text: &str) {
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
            wrap_row_into(&mut self.wrapped, row.kind, &row.text, width);
        }
    }

    pub(super) fn rewrap_stream(&mut self) {
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

    pub(super) fn rewrap_all(&mut self) {
        self.wrapped.clear();
        if self.tool_burst.row_open && !self.rows.is_empty() {
            let last = self.rows.len() - 1;
            let width = self.width;
            if width > 0 {
                for row in &self.rows[..last] {
                    wrap_row_into(&mut self.wrapped, row.kind, &row.text, width);
                }
            }
            self.tool_burst.wrap_at = self.wrapped.len();
            if width > 0 {
                let last = &self.rows[last];
                wrap_row_into(&mut self.wrapped, last.kind, &last.text, width);
            }
        } else {
            self.wrap_rows_from(0);
        }
        self.rewrap_stream();
        self.clear_selection();
    }

    fn clear_selection(&mut self) {
        self.select_anchor = None;
        self.select_head = None;
        self.dragging = false;
    }

    fn begin_selection(&mut self, column: u16, row: u16) {
        if self.total_lines() == 0 {
            self.clear_selection();
            return;
        }
        let pos = self.pos_at(column, row);
        self.select_anchor = Some(pos);
        self.select_head = Some(pos);
        self.dragging = true;
    }

    fn update_selection(&mut self, column: u16, row: u16) {
        if self.total_lines() == 0 {
            return;
        }
        self.select_head = Some(self.pos_at(column, row));
    }

    fn end_selection(&mut self) -> bool {
        self.dragging = false;
        match self.selected_text() {
            Some(text) if !text.is_empty() && copy_to_clipboard(&text) => true,
            _ => {
                self.clear_selection();
                false
            }
        }
    }

    fn pos_at(&self, column: u16, row: u16) -> SelPos {
        let total = self.total_lines();
        if total == 0 {
            return SelPos::default();
        }
        let top = self.current_top();
        let height = self.area.height.max(1);
        let rel_y = if row <= self.area.y {
            0
        } else {
            usize::from(row.saturating_sub(self.area.y)).min(usize::from(height - 1))
        };
        let raw_line = top.saturating_add(rel_y);
        let last = total - 1;
        let line = raw_line.min(last);
        let width = self.line_at(line).map(line_display_width).unwrap_or(0);
        let rel_x = if column <= self.area.x {
            0
        } else {
            usize::from(column.saturating_sub(self.area.x))
        };
        let col = if raw_line > last {
            width
        } else {
            rel_x.min(width)
        };
        SelPos { line, col }
    }

    fn line_at(&self, idx: usize) -> Option<&Line<'static>> {
        if idx < self.wrapped.len() {
            Some(&self.wrapped[idx])
        } else {
            self.wrapped_stream.get(idx - self.wrapped.len())
        }
    }

    fn normalized_sel(&self) -> Option<(SelPos, SelPos)> {
        let a = self.select_anchor?;
        let b = self.select_head?;
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        if start == end {
            None
        } else {
            Some((start, end))
        }
    }

    fn is_live_thinking(&self) -> bool {
        self.stream_kind == LineKind::Thinking && !self.streaming.is_empty()
    }

    fn is_live_text(&self) -> bool {
        self.stream_kind == LineKind::Text && !self.streaming.is_empty()
    }

    pub(super) fn selected_text(&self) -> Option<String> {
        let (start, end) = self.normalized_sel()?;
        let mut out = String::new();
        for idx in start.line..=end.line {
            let Some(line) = self.line_at(idx) else {
                break;
            };
            let from = if idx == start.line { start.col } else { 0 };
            let to = if idx == end.line {
                end.col
            } else {
                line_display_width(line)
            };
            if idx > start.line {
                out.push('\n');
            }
            out.push_str(&extract_line_range(line, from, to));
        }
        if out.is_empty() { None } else { Some(out) }
    }
}

impl Component for Transcript {
    fn handle_key(&mut self, key: KeyEvent, _state: &State) -> KeyResult {
        match key.code {
            KeyCode::PageUp => {
                self.scroll_up(self.view_height.max(1) as u16);
                KeyResult::Handled
            }
            KeyCode::PageDown => {
                self.scroll_down(self.view_height.max(1) as u16);
                KeyResult::Handled
            }
            _ => KeyResult::Ignored,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, _state: &State) -> KeyResult {
        let in_area = self.area.contains(ratatui::layout::Position {
            x: mouse.column,
            y: mouse.row,
        });
        match mouse.kind {
            MouseEventKind::ScrollUp if in_area => {
                self.scroll_up(MOUSE_SCROLL_STEP);
                KeyResult::Handled
            }
            MouseEventKind::ScrollDown if in_area => {
                self.scroll_down(MOUSE_SCROLL_STEP);
                KeyResult::Handled
            }
            MouseEventKind::Down(MouseButton::Left) if in_area => {
                self.begin_selection(mouse.column, mouse.row);
                KeyResult::Handled
            }
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved if self.dragging => {
                self.update_selection(mouse.column, mouse.row);
                KeyResult::Handled
            }
            MouseEventKind::Up(MouseButton::Left) if self.dragging => {
                self.update_selection(mouse.column, mouse.row);
                if self.end_selection() {
                    KeyResult::Action(Action::Notify("Copied!".into()))
                } else {
                    KeyResult::Handled
                }
            }
            _ => KeyResult::Ignored,
        }
    }

    fn on_event(&mut self, ev: &AppEvent) {
        match &ev.kind {
            AppEventKind::Agent(env) => match &env.event {
                AgentEvent::Stream(StreamEvent::ThinkingDelta { .. }) => {
                    self.close_tool_burst();
                    if self.stream_kind != LineKind::Thinking || self.streaming.is_empty() {
                        self.push_stream(LineKind::Thinking, THINKING_LABEL);
                    }
                }
                AgentEvent::Stream(StreamEvent::TextDelta { text }) => {
                    self.close_tool_burst();
                    self.push_stream(LineKind::Text, text);
                }
                AgentEvent::Tool(ToolEvent::Started { call_id, view, .. }) => {
                    self.flush_streaming();
                    self.note_tool_start(&call_id.0.to_string(), view);
                }
                AgentEvent::Tool(ToolEvent::Finished { call_id, result }) => {
                    let (ok, output) = match result {
                        ToolResult::Success { output } => (true, output.as_str()),
                        ToolResult::Failed { output, error } => {
                            (false, output.as_deref().unwrap_or(error))
                        }
                        ToolResult::Cancelled => (false, "cancelled"),
                    };
                    self.note_tool_end(&call_id.0.to_string(), ok, output);
                }
                AgentEvent::Tool(ToolEvent::OutputDelta { .. }) => {}
                AgentEvent::Turn(TurnEvent::Started) => {}
                AgentEvent::Turn(TurnEvent::Completed { .. }) => {
                    self.close_tool_burst();
                    self.flush_streaming();
                    self.push_separator();
                }
                AgentEvent::Turn(TurnEvent::Cancelled) => {
                    self.close_tool_burst();
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
                    self.push_separator();
                }
                AgentEvent::Turn(TurnEvent::Failed { error }) => {
                    self.close_tool_burst();
                    self.flush_streaming();
                    self.push_row(LineKind::Error, &error.message);
                    self.push_separator();
                }
                AgentEvent::TodosChanged { .. } => {}
            },
            AppEventKind::StateChanged(_) => {}
            AppEventKind::Exited => {}
            AppEventKind::Notification { .. } => {}
            AppEventKind::Error { message } => {
                self.close_tool_burst();
                self.flush_streaming();
                self.push_row(LineKind::Error, message);
                self.push_separator();
            }
        }
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect, state: &State) {
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
        let mut visible = collect_lines(&self.wrapped, &self.wrapped_stream, start, end);
        if self.is_live_thinking() {
            let phase = thinking_phase();
            let stream_start = self.wrapped.len();
            for (i, line) in visible.iter_mut().enumerate() {
                if start + i >= stream_start {
                    *line = apply_thinking_shimmer(line, phase);
                }
            }
        }
        if self.is_live_text()
            && (state.frame / CARET_FRAMES).is_multiple_of(2)
            && let Some(last) = visible.last_mut()
        {
            last.spans
                .push(Span::styled(STREAM_CARET, theme::assistant()));
        }
        if let Some((sel_start, sel_end)) = self.normalized_sel() {
            for (i, line) in visible.iter_mut().enumerate() {
                let idx = start + i;
                if idx < sel_start.line || idx > sel_end.line {
                    continue;
                }
                let width = line_display_width(line);
                let from = if idx == sel_start.line {
                    sel_start.col
                } else {
                    0
                };
                let to = if idx == sel_end.line {
                    sel_end.col
                } else {
                    width
                };
                *line = highlight_line(line, from, to);
            }
        }
        paint_visible(f, area, visible);
    }
}

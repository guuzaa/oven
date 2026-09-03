use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::super::collapsible::Collapsible;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use oven_app::{
    AgentEvent, AppEvent, AppEventKind, LocalShell, ShellEvent, StreamEvent, ToolEvent, ToolResult,
    ToolView, TurnEvent, display_shell_line, present_tool,
};
use oven_llm::{ContentBlock, Message, Role};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::super::component::{Action, Component, KeyResult, State};
use super::super::theme;
use super::kinds::{LineKind, Row};
use super::selection::{SelPos, copy_to_clipboard, extract_line_range, highlight_line};
use super::tools::{ToolBurst, ToolLabel};
use super::wrap::{
    MAX_SHELL_DISPLAY_LINES, THINKING_LABEL, THOUGHT_LABEL, apply_hover, apply_thinking_shimmer,
    collect_lines, format_lines, line_display_width, paint_visible, tail_lines, thinking_phase,
    trim_message, truncate_result, wrap_collapsible_thinking_into, wrap_line_into, wrap_row_into,
};

const MOUSE_SCROLL_STEP: u16 = 3;
const STREAM_CARET: &str = "▊";
const CARET_FRAMES: u64 = 5;
const DOUBLE_CLICK_TIMEOUT: Duration = Duration::from_millis(500);

pub struct Transcript {
    pub(super) rows: Vec<Row>,
    pub(super) wrapped: Vec<Line<'static>>,
    thinking_headers: Vec<Option<usize>>,
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
    pressed_thinking_header: Option<usize>,
    hovered_thinking: Option<usize>,
    last_thinking_click: Option<(usize, Instant)>,
    tool_burst: ToolBurst,
    detail_ids: HashSet<String>,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            wrapped: Vec::new(),
            thinking_headers: Vec::new(),
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
            pressed_thinking_header: None,
            hovered_thinking: None,
            last_thinking_click: None,
            tool_burst: ToolBurst::default(),
            detail_ids: HashSet::new(),
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.close_tool_burst();
        self.push_row(LineKind::User, text);
    }

    pub fn push_shell_command(&mut self, command: &str) {
        self.close_tool_burst();
        self.push_row(LineKind::Shell, command);
    }

    pub fn push_shell_output(&mut self, output: &str, ok: bool) {
        self.close_tool_burst();
        let trimmed = trim_message(output);
        let body = if trimmed.is_empty() {
            "(no output)".to_string()
        } else {
            trimmed
        };
        self.push_row(LineKind::ShellResult(ok), &body);
    }

    pub(crate) fn last_user_text(&self) -> Option<String> {
        self.rows.iter().rev().find_map(|r| match r.kind {
            LineKind::User => Some(r.text.clone()),
            LineKind::Shell => Some(display_shell_line(&r.text)),
            _ => None,
        })
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
                                if let Some(sh) = LocalShell::try_parse(text) {
                                    self.push_shell_command(&sh.command);
                                    self.push_shell_output(&sh.output, sh.ok());
                                } else {
                                    self.push_row(LineKind::User, text);
                                }
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
                            ContentBlock::Thinking { thinking } => {
                                self.close_tool_burst();
                                self.push_thinking(THOUGHT_LABEL, thinking);
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
        self.thinking_headers.clear();
        self.clear_stream();
        self.pinned = true;
        self.top = 0;
        self.clear_selection();
        self.hovered_thinking = None;
        self.last_thinking_click = None;
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
            let kind = if view.diff {
                LineKind::Diff
            } else {
                LineKind::Tool
            };
            self.push_row(kind, &view.summary);
            return;
        }
        let label = ToolLabel::from_summary(&view.summary);
        self.tool_burst.start(call_id.to_string(), label);
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
        if self.tool_burst.finish(call_id, !ok) && !ok {
            self.upsert_tool_summary();
        }
    }

    fn note_seed_result(&mut self, tool_use_id: &str, is_error: bool, content: &[ContentBlock]) {
        if self.detail_ids.remove(tool_use_id) {
            self.push_tool_result(is_error, content);
            return;
        }
        if self.tool_burst.finish(tool_use_id, is_error) && is_error {
            self.upsert_tool_summary();
        }
    }

    fn upsert_tool_summary(&mut self) {
        let text = self.tool_burst.summary();
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
            LineKind::ShellResult(_) => tail_lines(text, MAX_SHELL_DISPLAY_LINES),
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
            LineKind::ShellResult(_) => tail_lines(text, MAX_SHELL_DISPLAY_LINES),
            LineKind::Separator => String::new(),
            _ => text.to_string(),
        };
        self.push_row_with_detail(kind, text, None);
    }

    fn push_thinking(&mut self, title: &str, text: &str) {
        if let Some(Row {
            kind: LineKind::Thinking,
            text: current_title,
            collapsible: Some(collapsible),
        }) = self.rows.last_mut()
        {
            *current_title = title.to_string();
            collapsible.append(text);
            self.rewrap_all();
            self.keep_following();
            return;
        }
        self.push_row_with_detail(
            LineKind::Thinking,
            title.to_string(),
            Some(Collapsible::new(text)),
        );
    }

    fn push_row_with_detail(
        &mut self,
        kind: LineKind,
        text: String,
        collapsible: Option<Collapsible>,
    ) {
        let row = Row {
            kind,
            text,
            collapsible,
        };
        let header = if self.width > 0 {
            Self::wrap_row_into(&mut self.wrapped, &row, self.width)
        } else {
            None
        };
        self.rows.push(row);
        self.thinking_headers.push(header);
        self.keep_following();
    }

    fn wrap_row_into(out: &mut Vec<Line<'static>>, row: &Row, width: usize) -> Option<usize> {
        let start = out.len();
        if row.kind == LineKind::Thinking
            && let Some(collapsible) = &row.collapsible
        {
            let header = start + usize::from(start > 0);
            wrap_collapsible_thinking_into(out, &row.text, collapsible, width);
            Some(header)
        } else {
            wrap_row_into(out, row.kind, &row.text, width);
            None
        }
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

    fn wrap_rows(&mut self, start: usize, end: usize) {
        if self.width == 0 {
            return;
        }
        for row in &self.rows[start..end] {
            let header = Self::wrap_row_into(&mut self.wrapped, row, self.width);
            self.thinking_headers.push(header);
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
        self.thinking_headers.clear();
        if self.tool_burst.row_open && !self.rows.is_empty() {
            let last = self.rows.len() - 1;
            self.wrap_rows(0, last);
            self.tool_burst.wrap_at = self.wrapped.len();
            self.wrap_rows(last, self.rows.len());
        } else {
            self.wrap_rows(0, self.rows.len());
        }
        self.rewrap_stream();
        self.clear_selection();
    }

    fn clear_selection(&mut self) {
        self.select_anchor = None;
        self.select_head = None;
        self.dragging = false;
        self.pressed_thinking_header = None;
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

    fn thinking_header_at(&self, column: u16, row: u16) -> Option<usize> {
        let line = self.pos_at(column, row).line;
        self.thinking_headers
            .iter()
            .position(|header| *header == Some(line))
    }

    fn update_hover(&mut self, in_area: bool, column: u16, row: u16) -> bool {
        let hovered = in_area
            .then(|| self.thinking_header_at(column, row))
            .flatten();
        let changed = self.hovered_thinking != hovered;
        self.hovered_thinking = hovered;
        changed
    }

    fn toggle_thinking(&mut self, row: usize) {
        if let Some(collapsible) = self.rows[row].collapsible.as_mut() {
            collapsible.toggle();
            self.rewrap_all();
            self.keep_following();
        }
    }

    fn finish_thinking(&mut self) {
        if let Some(Row {
            kind: LineKind::Thinking,
            text,
            collapsible: Some(_),
        }) = self.rows.last_mut()
        {
            *text = THOUGHT_LABEL.to_string();
            self.rewrap_all();
        }
    }

    fn live_thinking_header(&self) -> Option<usize> {
        matches!(
            self.rows.last(),
            Some(Row {
                kind: LineKind::Thinking,
                text,
                collapsible: Some(_),
            }) if text == THINKING_LABEL
        )
        .then(|| self.thinking_headers.last().copied().flatten())
        .flatten()
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
                self.update_hover(in_area, mouse.column, mouse.row);
                KeyResult::Handled
            }
            MouseEventKind::ScrollDown if in_area => {
                self.scroll_down(MOUSE_SCROLL_STEP);
                self.update_hover(in_area, mouse.column, mouse.row);
                KeyResult::Handled
            }
            MouseEventKind::Down(MouseButton::Left) if in_area => {
                let header = self.thinking_header_at(mouse.column, mouse.row);
                if let Some(row) = header
                    && let Some((last, at)) = self.last_thinking_click
                    && last == row
                    && at.elapsed() <= DOUBLE_CLICK_TIMEOUT
                {
                    self.last_thinking_click = None;
                    self.toggle_thinking(row);
                    self.clear_selection();
                    return KeyResult::Handled;
                }
                if header.is_none() {
                    self.last_thinking_click = None;
                }
                self.pressed_thinking_header = header;
                self.begin_selection(mouse.column, mouse.row);
                KeyResult::Handled
            }
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved if self.dragging => {
                self.update_selection(mouse.column, mouse.row);
                KeyResult::Handled
            }
            MouseEventKind::Moved => {
                if self.update_hover(in_area, mouse.column, mouse.row) {
                    KeyResult::Handled
                } else {
                    KeyResult::Ignored
                }
            }
            MouseEventKind::Up(MouseButton::Left) if self.dragging => {
                self.update_selection(mouse.column, mouse.row);
                let header = self.pressed_thinking_header;
                let selected = self.normalized_sel().is_some();
                if self.end_selection() {
                    KeyResult::Action(Action::Notify("Copied!".into()))
                } else {
                    if !selected {
                        self.last_thinking_click = header.map(|row| (row, Instant::now()));
                    } else {
                        self.last_thinking_click = None;
                    }
                    KeyResult::Handled
                }
            }
            _ => KeyResult::Ignored,
        }
    }

    fn on_event(&mut self, ev: &AppEvent) {
        match &ev.kind {
            AppEventKind::Agent(env) => match &env.event {
                AgentEvent::Stream(StreamEvent::ThinkingDelta { text }) => {
                    self.close_tool_burst();
                    self.push_thinking(THINKING_LABEL, text);
                }
                AgentEvent::Stream(StreamEvent::TextDelta { text }) => {
                    self.close_tool_burst();
                    self.finish_thinking();
                    self.push_stream(LineKind::Text, text);
                }
                AgentEvent::Tool(ToolEvent::Started { call_id, view, .. }) => {
                    self.finish_thinking();
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
                    self.finish_thinking();
                    self.flush_streaming();
                    self.push_separator();
                }
                AgentEvent::Turn(TurnEvent::Cancelled) => {
                    self.close_tool_burst();
                    self.finish_thinking();
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
                    self.finish_thinking();
                    self.flush_streaming();
                    self.push_row(LineKind::Error, &error.message);
                    self.push_separator();
                }
                AgentEvent::TodosChanged { .. } => {}
            },
            AppEventKind::Shell(ev) => match ev {
                ShellEvent::Started { .. } => {}
                ShellEvent::Finished {
                    output, exit_code, ..
                } => self.push_shell_output(output, *exit_code == 0),
                ShellEvent::Failed { error, output, .. } => {
                    let body = if output.is_empty() { error } else { output };
                    self.push_shell_output(body, false);
                }
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
        if let Some(header) = self.live_thinking_header()
            && header >= start
            && let Some(line) = visible.get_mut(header - start)
        {
            *line = apply_thinking_shimmer(line, thinking_phase());
        }
        if let Some(row) = self.hovered_thinking
            && let Some(Some(header)) = self.thinking_headers.get(row)
            && *header >= start
            && let Some(line) = visible.get_mut(header - start)
        {
            *line = apply_hover(line, width);
        }
        if self.is_live_text()
            && end == total
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

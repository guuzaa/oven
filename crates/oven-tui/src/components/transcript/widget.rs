use std::collections::HashMap;
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
    MAX_SHELL_DISPLAY_LINES, RESULT_LABEL, THINKING_LABEL, THOUGHT_LABEL, apply_hover,
    apply_thinking_shimmer, collect_lines, format_elapsed, format_lines, format_thought,
    line_display_width, paint_visible, tail_lines, thinking_phase, trim_message,
    wrap_collapsible_into, wrap_line_into, wrap_row_into,
};

const MOUSE_SCROLL_STEP: u16 = 3;
const STREAM_CARET: &str = "▊";
const CARET_FRAMES: u64 = 5;
const DOUBLE_CLICK_TIMEOUT: Duration = Duration::from_millis(500);
const NO_OUTPUT: &str = "(no output)";

pub struct Transcript {
    pub(super) rows: Vec<Row>,
    pub(super) wrapped: Vec<Line<'static>>,
    streaming: String,
    stream_kind: LineKind,
    wrapped_stream: Vec<Line<'static>>,
    /// None follows newest content; Some is an anchored wrapped-line index.
    pub(super) top: Option<usize>,
    pub(super) area: Rect,
    pub(super) select_anchor: Option<SelPos>,
    select_head: Option<SelPos>,
    pub(super) dragging: bool,
    hovered_collapsible: Option<usize>,
    last_collapsible_click: Option<(usize, Instant)>,
    tool_burst: ToolBurst,
    detail_ids: HashMap<String, bool>,
    thinking_started: Option<Instant>,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            wrapped: Vec::new(),
            streaming: String::new(),
            stream_kind: LineKind::Text,
            wrapped_stream: Vec::new(),
            top: None,
            area: Rect::default(),
            select_anchor: None,
            select_head: None,
            dragging: false,
            hovered_collapsible: None,
            last_collapsible_click: None,
            tool_burst: ToolBurst::default(),
            detail_ids: HashMap::new(),
            thinking_started: None,
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.close_tool_burst();
        self.push_row(LineKind::User, text);
        self.top = None;
    }

    pub fn push_shell_command(&mut self, command: &str) {
        self.close_tool_burst();
        self.push_row(LineKind::Shell, command);
        self.top = None;
    }

    pub fn push_shell_output(&mut self, output: &str, ok: bool) {
        self.close_tool_burst();
        let trimmed = trim_message(output);
        self.push_row(
            LineKind::ShellResult(ok),
            if trimmed.is_empty() {
                NO_OUTPUT
            } else {
                &trimmed
            },
        );
    }

    pub(crate) fn last_user_text(&self) -> Option<String> {
        self.rows.iter().rev().find_map(|r| match r.kind {
            LineKind::User => Some(r.text.clone()),
            LineKind::Shell => Some(display_shell_line(&r.text)),
            _ => None,
        })
    }

    #[cfg(test)]
    pub(crate) fn replace_from(&mut self, messages: &[Message]) {
        self.replace_from_timed(&timed_messages(messages));
    }

    pub(crate) fn replace_from_timed(&mut self, messages: &[(Message, u64, Option<u64>)]) {
        self.reset();
        self.seed_timed(messages);
    }

    /// Pre-fill the transcript from a persisted session's messages when
    /// resuming. Renders the same row kinds the live event stream produces;
    /// images are skipped since they cannot be drawn in a terminal.
    #[cfg(test)]
    pub fn seed(&mut self, messages: &[Message]) {
        self.seed_timed(&timed_messages(messages));
    }

    pub fn seed_timed(&mut self, messages: &[(Message, u64, Option<u64>)]) {
        let mut turn_started_at: Option<u64> = None;
        for (m, ts, thinking_ms) in messages {
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
                                    turn_started_at = Some(*ts);
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
                                self.push_thinking(
                                    &format_thought(thinking_ms.unwrap_or(0)),
                                    thinking,
                                );
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
                        self.push_elapsed(turn_elapsed(turn_started_at, *ts));
                    }
                }
                Role::System => {}
            }
        }
        self.close_tool_burst();
        self.collapse_open();
    }

    fn push_tool_result(&mut self, is_error: bool, content: &[ContentBlock]) {
        let output = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.push_result_row(!is_error, &output);
    }

    fn push_result_row(&mut self, ok: bool, output: &str) {
        let body = trim_message(output);
        if ok && body.is_empty() {
            return;
        }
        self.push_row(
            LineKind::ToolResult(ok),
            if body.is_empty() { NO_OUTPUT } else { &body },
        );
    }

    pub(super) fn total_lines(&self) -> usize {
        self.wrapped.len() + self.wrapped_stream.len()
    }

    fn width(&self) -> usize {
        self.area.width as usize
    }

    fn height(&self) -> usize {
        self.area.height as usize
    }

    pub(super) fn current_top(&self) -> usize {
        self.top
            .unwrap_or_else(|| self.total_lines().saturating_sub(self.height()))
    }

    pub(super) fn scroll_up(&mut self, n: u16) {
        self.top = Some(self.current_top().saturating_sub(n as usize));
    }

    pub(super) fn scroll_down(&mut self, n: u16) {
        let total = self.total_lines();
        let height = self.height().max(1);
        let max_top = total.saturating_sub(height);
        let top = self.current_top().saturating_add(n as usize).min(max_top);
        self.top = (top.saturating_add(height) < total).then_some(top);
    }

    pub(super) fn reset(&mut self) {
        self.rows.clear();
        self.wrapped.clear();
        self.clear_stream();
        self.top = None;
        self.clear_selection();
        self.hovered_collapsible = None;
        self.last_collapsible_click = None;
        self.close_tool_burst();
        self.detail_ids.clear();
        self.thinking_started = None;
    }

    fn close_tool_burst(&mut self) {
        self.tool_burst = ToolBurst::default();
    }

    fn note_tool_start(&mut self, call_id: &str, view: &ToolView) {
        if !view.collapse {
            self.close_tool_burst();
            self.detail_ids.insert(call_id.to_string(), view.diff);
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
        if let Some(is_diff) = self.detail_ids.remove(call_id) {
            if !is_diff || !ok {
                self.push_result_row(ok, output);
            }
            return;
        }
        if self.tool_burst.finish(call_id, !ok) && !ok {
            self.upsert_tool_summary();
        }
    }

    fn note_seed_result(&mut self, tool_use_id: &str, is_error: bool, content: &[ContentBlock]) {
        if let Some(is_diff) = self.detail_ids.remove(tool_use_id) {
            if !is_diff || is_error {
                self.push_tool_result(is_error, content);
            }
            return;
        }
        if self.tool_burst.finish(tool_use_id, is_error) && is_error {
            self.upsert_tool_summary();
        }
    }

    fn upsert_tool_summary(&mut self) {
        let text = self.tool_burst.summary();
        if self.tool_burst.row_open {
            if let Some(last) = self.rows.last_mut() {
                last.text = text;
            }
            self.rewrap_all();
        } else {
            self.push_row(LineKind::Tool, &text);
            self.tool_burst.row_open = true;
        }
    }

    pub(super) fn push_row(&mut self, kind: LineKind, text: &str) {
        let (text, collapsible) = match kind {
            LineKind::ToolResult(_) => (RESULT_LABEL.to_string(), Some(Collapsible::new(text))),
            LineKind::Thinking => (THOUGHT_LABEL.to_string(), None),
            LineKind::ShellResult(_) => (tail_lines(text, MAX_SHELL_DISPLAY_LINES), None),
            LineKind::Diff => {
                let (title, body) = match text.split_once('\n') {
                    Some((title, body)) => (title.to_string(), body.to_string()),
                    None => (text.to_string(), String::new()),
                };
                (title, Some(Collapsible::new(body)))
            }
            _ => (text.to_string(), None),
        };
        self.push_row_with_detail(kind, text, collapsible);
    }

    fn push_thinking(&mut self, title: &str, text: &str) {
        if let Some(Row {
            kind: LineKind::Thinking,
            text: current_title,
            collapsible: Some(collapsible),
            ..
        }) = self.rows.last_mut()
        {
            *current_title = title.to_string();
            collapsible.append(text);
            self.rewrap_all();
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
        if kind != LineKind::Separator {
            self.collapse_open();
        }
        self.append_row(kind, text, collapsible);
    }

    fn append_row(&mut self, kind: LineKind, text: String, collapsible: Option<Collapsible>) {
        self.rows.push(Row {
            kind,
            text,
            collapsible,
            header: None,
        });
        self.wrap_row(self.rows.len() - 1);
    }

    fn collapse_open(&mut self) {
        let mut changed = false;
        for row in &mut self.rows {
            if let Some(collapsible) = row.collapsible.as_mut() {
                changed |= collapsible.collapse();
            }
        }
        if changed {
            self.rewrap_all();
        }
    }

    fn wrap_row_into(out: &mut Vec<Line<'static>>, row: &Row, width: usize) -> Option<usize> {
        let start = out.len();
        if let Some(collapsible) = &row.collapsible {
            let header = start + usize::from(start > 0);
            wrap_collapsible_into(out, row.kind, &row.text, collapsible, width);
            Some(header)
        } else {
            wrap_row_into(out, row.kind, &row.text, width);
            None
        }
    }

    fn push_separator(&mut self) {
        self.push_turn_end("");
    }

    fn push_elapsed(&mut self, duration_ms: u64) {
        if duration_ms == 0 {
            self.push_turn_end("");
            return;
        }
        self.push_turn_end(&format_elapsed(duration_ms));
    }

    fn push_turn_end(&mut self, text: &str) {
        if matches!(
            self.rows.last().map(|r| r.kind),
            Some(LineKind::Separator) | None
        ) {
            return;
        }
        self.push_row(LineKind::Separator, text);
    }

    fn take_stream(&mut self) -> (LineKind, String) {
        self.wrapped_stream.clear();
        (
            self.stream_kind,
            trim_message(&std::mem::take(&mut self.streaming)),
        )
    }

    fn flush_streaming(&mut self) {
        let (kind, body) = self.take_stream();
        if !body.is_empty() {
            self.append_row(kind, body, None);
        }
    }

    fn clear_stream(&mut self) {
        self.streaming.clear();
        self.wrapped_stream.clear();
    }

    pub(super) fn push_stream(&mut self, kind: LineKind, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.streaming.is_empty() && self.stream_kind != kind {
            self.flush_streaming();
        }
        if self.streaming.is_empty() {
            self.collapse_open();
        }
        self.stream_kind = kind;
        self.streaming.push_str(text);
    }

    fn wrap_row(&mut self, idx: usize) {
        let width = self.width();
        let header = if width == 0 {
            None
        } else {
            Self::wrap_row_into(&mut self.wrapped, &self.rows[idx], width)
        };
        self.rows[idx].header = header;
    }

    fn wrap_rows(&mut self, start: usize, end: usize) {
        for i in start..end {
            self.wrap_row(i);
        }
    }

    pub(super) fn rewrap_stream(&mut self) {
        self.wrapped_stream.clear();
        let width = self.width();
        if width == 0 || self.streaming.is_empty() {
            return;
        }
        if !self.wrapped.is_empty() {
            self.wrapped_stream.push(Line::from(""));
        }
        for line in format_lines(self.stream_kind, &self.streaming) {
            wrap_line_into(&mut self.wrapped_stream, &line, width);
        }
    }

    pub(super) fn rewrap_all(&mut self) {
        self.wrapped.clear();
        self.wrap_rows(0, self.rows.len());
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

    fn collapsible_header_at(&self, column: u16, row: u16) -> Option<usize> {
        let line = self.pos_at(column, row).line;
        self.rows.iter().position(|r| r.header == Some(line))
    }

    fn update_hover(&mut self, in_area: bool, column: u16, row: u16) -> bool {
        let hovered = in_area
            .then(|| self.collapsible_header_at(column, row))
            .flatten();
        let changed = self.hovered_collapsible != hovered;
        self.hovered_collapsible = hovered;
        changed
    }

    fn toggle_collapsible(&mut self, row: usize) {
        if let Some(collapsible) = self.rows[row].collapsible.as_mut() {
            collapsible.toggle();
            self.rewrap_all();
        }
    }

    fn finish_thinking(&mut self) {
        let ms = self
            .thinking_started
            .take()
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        if let Some(Row {
            kind: LineKind::Thinking,
            text,
            collapsible: Some(_),
            ..
        }) = self.rows.last_mut()
        {
            *text = format_thought(ms);
            self.rewrap_all();
        }
    }

    fn live_thinking_header(&self) -> Option<usize> {
        let last = self.rows.last()?;
        matches!(
            last,
            Row {
                kind: LineKind::Thinking,
                text,
                collapsible: Some(_),
                ..
            } if text == THINKING_LABEL
        )
        .then_some(last.header)
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
                self.scroll_up(self.height().max(1) as u16);
                KeyResult::Handled
            }
            KeyCode::PageDown => {
                self.scroll_down(self.height().max(1) as u16);
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
                let header = self.collapsible_header_at(mouse.column, mouse.row);
                if let Some(row) = header
                    && let Some((last, at)) = self.last_collapsible_click
                    && last == row
                    && at.elapsed() <= DOUBLE_CLICK_TIMEOUT
                {
                    self.last_collapsible_click = None;
                    self.toggle_collapsible(row);
                    self.clear_selection();
                    return KeyResult::Handled;
                }
                self.last_collapsible_click = header.map(|row| (row, Instant::now()));
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
                let selected = self.normalized_sel().is_some();
                let copied = self.end_selection();
                if copied || selected {
                    self.last_collapsible_click = None;
                }
                if copied {
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
                AgentEvent::Stream(StreamEvent::ThinkingDelta { text }) => {
                    self.close_tool_burst();
                    if self.thinking_started.is_none() {
                        self.thinking_started = Some(Instant::now());
                    }
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
                AgentEvent::Turn(TurnEvent::Completed { duration_ms, .. }) => {
                    self.close_tool_burst();
                    self.finish_thinking();
                    self.flush_streaming();
                    self.push_elapsed(*duration_ms);
                }
                AgentEvent::Turn(TurnEvent::Cancelled { duration_ms }) => {
                    self.close_tool_burst();
                    self.finish_thinking();
                    if !self.streaming.is_empty() {
                        let (kind, partial) = self.take_stream();
                        if !partial.is_empty() {
                            self.append_row(kind, format!("{partial}…"), None);
                        }
                    }
                    self.push_row(LineKind::System, "cancelled");
                    self.push_elapsed(*duration_ms);
                }
                AgentEvent::Turn(TurnEvent::Failed { error, duration_ms }) => {
                    self.close_tool_burst();
                    self.finish_thinking();
                    self.flush_streaming();
                    self.push_row(LineKind::Error, &error.message);
                    self.push_elapsed(*duration_ms);
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
            AppEventKind::Compaction(ev) => {
                if matches!(ev, oven_app::CompactionEvent::Completed { .. }) {
                    self.push_row(LineKind::System, "context compacted");
                    self.push_separator();
                }
            }
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
        let resized = area.width != self.area.width;
        self.area = area;
        if resized {
            self.rewrap_all();
        } else if !self.streaming.is_empty() {
            self.rewrap_stream();
        }

        let height = area.height as usize;
        let total = self.total_lines();
        let max_top = total.saturating_sub(height);
        if let Some(top) = self.top {
            let top = top.min(max_top);
            self.top = (top.saturating_add(height) < total).then_some(top);
        }
        let start = self.top.unwrap_or(max_top);
        let end = start.saturating_add(height).min(total);
        let mut visible = collect_lines(&self.wrapped, &self.wrapped_stream, start, end);
        if let Some(header) = self.live_thinking_header()
            && header >= start
            && let Some(line) = visible.get_mut(header - start)
        {
            *line = apply_thinking_shimmer(line, thinking_phase());
        }
        if let Some(row) = self.hovered_collapsible
            && let Some(header) = self.rows.get(row).and_then(|r| r.header)
            && header >= start
            && let Some(line) = visible.get_mut(header - start)
        {
            *line = apply_hover(line, self.width());
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

#[cfg(test)]
fn timed_messages(messages: &[Message]) -> Vec<(Message, u64, Option<u64>)> {
    messages.iter().cloned().map(|m| (m, 0, None)).collect()
}

fn turn_elapsed(started_at: Option<u64>, ended_at: u64) -> u64 {
    started_at
        .map(|start| ended_at.saturating_sub(start))
        .unwrap_or(0)
}

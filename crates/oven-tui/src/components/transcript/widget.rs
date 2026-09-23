use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::collapsible::Collapsible;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use oven_app::{
    AgentEvent, AppEvent, AppEventKind, LocalShell, ShellEvent, StreamEvent, ToolEvent, ToolResult,
    ToolView, TurnEvent, present_tool,
};
use oven_llm::{ContentBlock, Message, Role};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::super::component::{Action, Component, KeyResult, State};
use super::super::theme;

use super::kinds::{Header, LineKind, Row};
use super::selection::{SelPos, copy_to_clipboard, extract_line_range, highlight_line};
use super::tools::ToolBurst;
use super::wrap::{
    MAX_LIVE_BODY_ROWS, MAX_SHELL_DISPLAY_LINES, RESULT_LABEL, THINKING_LABEL, THOUGHT_LABEL,
    apply_hover, apply_thinking_shimmer, collect_lines, format_elapsed, format_lines,
    format_thought, line_display_width, paint_visible, tail_lines, thinking_phase, trim_message,
    wrap_collapsible_into, wrap_line_into, wrap_row_into,
};

const MOUSE_SCROLL_STEP: u16 = 3;
const STREAM_CARET: &str = "▊";
const CARET_FRAMES: u64 = 5;
const DOUBLE_CLICK_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_STICKY_PROMPT_ROWS: usize = 6;
const NO_OUTPUT: &str = "(no output)";
pub(super) const LOOP_LIMIT_REACHED: &str = "agent loop limit reached";

pub struct Transcript {
    pub(super) rows: Vec<Row>,
    pub(super) wrapped: Vec<Line<'static>>,
    streaming: String,
    stream_kind: LineKind,
    pub(super) wrapped_stream: Vec<Line<'static>>,
    /// None follows newest content; Some is an anchored wrapped-line index.
    pub(super) top: Option<usize>,
    pub(super) area: Rect,
    pub(super) select_anchor: Option<SelPos>,
    select_head: Option<SelPos>,
    pub(super) dragging: bool,
    hovered_collapsible: Option<Header>,
    last_collapsible_click: Option<(Header, Instant)>,
    tool_burst: ToolBurst,
    burst_row: Option<usize>,
    /// Calls rendered as their own rows; `true` when they carry a detail
    /// body, whose success output stays hidden.
    detail_ids: HashMap<String, bool>,
    thinking_row: Option<usize>,
    active_prompt: Option<usize>,
    sticky_prompt: Option<(usize, u16)>,
    render_start: usize,
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
            burst_row: None,
            detail_ids: HashMap::new(),
            thinking_row: None,
            active_prompt: None,
            sticky_prompt: None,
            render_start: 0,
        }
    }

    /// Appends a user row to the transcript.
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
        self.push_row(
            LineKind::ShellResult(ok),
            if trimmed.is_empty() {
                NO_OUTPUT
            } else {
                &trimmed
            },
        );
    }

    /// Starts a turn at the tail by appending its prompt before any response
    /// events can arrive.
    pub(crate) fn start_user_turn(&mut self, text: &str) {
        self.push_user(text);
        self.active_prompt = Some(self.rows.len() - 1);
        self.top = None;
    }

    /// Starts a local shell turn at the tail.
    pub(crate) fn start_shell_turn(&mut self, command: &str) {
        self.push_shell_command(command);
        self.active_prompt = Some(self.rows.len() - 1);
        self.top = None;
    }

    #[cfg(test)]
    pub(super) fn has_active_prompt(&self) -> bool {
        self.active_prompt.is_some()
    }

    #[cfg(test)]
    pub(super) fn has_sticky_prompt(&self) -> bool {
        self.sticky_prompt.is_some()
    }

    pub(crate) fn rewind_text(&self) -> Option<String> {
        let row = self
            .rows
            .iter()
            .rfind(|row| matches!(row.kind, LineKind::User | LineKind::Shell))?;
        Some(if row.kind == LineKind::Shell {
            oven_app::display_shell_line(&row.text)
        } else {
            row.text.clone()
        })
    }

    /// Closes the current response the way a completed turn does, without
    /// touching the pinned prompt — for paths that end a turn without the
    /// agent reporting one.
    pub(crate) fn finish_response(&mut self) {
        self.close_tool_burst();
        self.stop_live_thinking();
        self.flush_streaming();
        self.active_prompt = None;
    }

    /// Rebuilds the rows from a history without promoting the last user
    /// message, the way the pinned prompt path does.
    #[cfg(test)]
    pub(crate) fn replace_from(&mut self, messages: &[Message]) {
        let mut fresh = Self::new();
        fresh.seed_timed(&timed_messages(messages));
        *self = fresh;
    }

    /// Pre-fill the transcript from a persisted session's messages when
    /// resuming. Renders the same row kinds the live event stream produces;
    /// images are skipped since they cannot be drawn in a terminal.
    #[cfg(test)]
    pub fn seed(&mut self, messages: &[Message]) {
        self.seed_timed(&timed_messages(messages));
    }

    pub fn seed_timed(&mut self, messages: &[(Arc<Message>, u64, Option<u64>)]) {
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
                    // Providers that open the text block first (an empty
                    // leading `content` delta) persist the reasoning after the
                    // answer; live events always put reasoning first, so the
                    // seeded transcript renders the same order.
                    let is_thinking =
                        |block: &ContentBlock| matches!(block, ContentBlock::Thinking { .. });
                    let blocks = m
                        .content
                        .iter()
                        .filter(|block| is_thinking(block))
                        .chain(m.content.iter().filter(|block| !is_thinking(block)));
                    for block in blocks {
                        match block {
                            ContentBlock::Thinking { thinking } => {
                                self.close_tool_burst();
                                self.push_thinking(&format_thought(*thinking_ms), thinking);
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
                            ContentBlock::ToolUse {
                                id, name, input, ..
                            } => {
                                self.note_tool_start(id, &present_tool(name, input));
                                emitted = true;
                                has_tool = true;
                            }
                            _ => {}
                        }
                    }
                    // A message ending in a tool call is mid-turn: the answer
                    // it led to is still to come, so no turn end yet.
                    if emitted && !has_tool {
                        self.push_elapsed(turn_elapsed(turn_started_at, *ts));
                    }
                }
                Role::System => {}
            }
        }
        self.close_tool_burst();
        // Seeded rows carry their durations already, so none of them is still
        // awaiting the live clock that would keep its body windowed.
        self.thinking_row = None;
        self.collapse_open();
    }

    fn push_tool_result(&mut self, is_error: bool, content: &[ContentBlock]) {
        self.push_result_row(!is_error, &result_text(content));
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

    fn close_tool_burst(&mut self) {
        self.tool_burst = ToolBurst::default();
        if let Some(row) = self.burst_row.take() {
            if let Some(collapsible) = self.rows[row].collapsible.as_mut() {
                collapsible.collapse();
            }
            self.rewrap_all();
        }
    }

    fn note_tool_start(&mut self, call_id: &str, view: &ToolView) {
        if !view.collapse {
            self.close_tool_burst();
            self.detail_ids
                .insert(call_id.to_string(), view.detail.is_some());
            let kind = if view.detail.is_some() {
                LineKind::Diff
            } else {
                LineKind::Tool
            };
            let detail = view.detail.as_deref().map(Collapsible::new);
            self.push_row_with_detail(kind, view.summary.clone(), detail);
            return;
        }
        self.tool_burst
            .start(call_id.to_string(), &view.summary, view.detail.as_deref());
        self.upsert_tool_summary();
    }

    fn note_tool_end(&mut self, call_id: &str, ok: bool, output: &str) {
        if self
            .tool_burst
            .finish(call_id, !ok, (!ok).then_some(output))
        {
            if !ok {
                self.upsert_tool_summary();
            }
            return;
        }
        if let Some(has_detail) = self.detail_ids.remove(call_id) {
            if !has_detail || !ok {
                self.push_result_row(ok, output);
            }
            return;
        }
        if !ok {
            self.push_row(LineKind::System, output);
        }
    }

    fn note_seed_result(&mut self, tool_use_id: &str, is_error: bool, content: &[ContentBlock]) {
        if let Some(has_detail) = self.detail_ids.remove(tool_use_id) {
            if !has_detail || is_error {
                self.push_tool_result(is_error, content);
            }
            return;
        }
        let error = is_error.then(|| result_text(content));
        if self
            .tool_burst
            .finish(tool_use_id, is_error, error.as_deref())
            && is_error
        {
            self.upsert_tool_summary();
        }
    }

    fn upsert_tool_summary(&mut self) {
        let title = self.tool_burst.title();
        let sections = self.tool_burst.sections();
        if let Some(row) = self.burst_row.and_then(|idx| self.rows.get_mut(idx)) {
            row.text = title;
            if let Some(collapsible) = row.collapsible.as_mut() {
                collapsible.replace_sections(sections);
            }
        } else {
            self.push_row_with_detail(
                LineKind::Tool,
                title,
                Some(Collapsible::from_sections(sections)),
            );
            self.burst_row = Some(self.rows.len() - 1);
        }
        self.rewrap_all();
    }

    pub(super) fn push_row(&mut self, kind: LineKind, text: &str) {
        let (text, collapsible) = match kind {
            LineKind::ToolResult(_) => (RESULT_LABEL.to_string(), Some(Collapsible::new(text))),
            LineKind::Thinking => (THOUGHT_LABEL.to_string(), None),
            LineKind::ShellResult(_) => (tail_lines(text, MAX_SHELL_DISPLAY_LINES), None),
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
        self.collapse_open();
        self.append_row(kind, text, collapsible);
    }

    /// Appends the row and tracks it as the thinking row awaiting a duration.
    fn append_row(&mut self, kind: LineKind, text: String, collapsible: Option<Collapsible>) {
        self.rows.push(Row {
            kind,
            text,
            collapsible,
            headers: Vec::new(),
        });
        self.wrap_row(self.rows.len() - 1);
        if kind == LineKind::Thinking {
            self.thinking_row = Some(self.rows.len() - 1);
        }
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

    fn wrap_row_into(
        out: &mut Vec<Line<'static>>,
        row: &Row,
        width: usize,
        live_rows: Option<usize>,
    ) -> Vec<Header> {
        if let Some(collapsible) = &row.collapsible {
            wrap_collapsible_into(out, row.kind, &row.text, collapsible, width, live_rows)
        } else {
            wrap_row_into(out, row.kind, &row.text, width);
            Vec::new()
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
        let live_rows = self.live_body_rows(idx);
        let headers = if width == 0 {
            Vec::new()
        } else {
            Self::wrap_row_into(&mut self.wrapped, &self.rows[idx], width, live_rows)
        };
        self.rows[idx].headers = headers;
    }

    /// A body that is still growing — live thinking deltas or an open tool burst
    /// — renders only its newest screen rows, so it cannot scroll the view up.
    fn live_body_rows(&self, idx: usize) -> Option<usize> {
        let live = Some(idx) == self.thinking_row || Some(idx) == self.burst_row;
        live.then_some(MAX_LIVE_BODY_ROWS)
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
            wrap_line_into(&mut self.wrapped_stream, &line, width, self.stream_kind);
        }
    }

    pub(super) fn rewrap_all(&mut self) {
        self.wrapped.clear();
        self.wrap_rows(0, self.rows.len());
        self.rewrap_stream();
        self.reanchor_selection();
    }

    /// Events land mid-drag on every tool call, so a rewrap must never drop an
    /// in-progress selection: both ends are clamped into the new line range.
    fn reanchor_selection(&mut self) {
        let total = self.total_lines();
        if total == 0 {
            self.clear_selection();
            return;
        }
        let last = total - 1;
        let anchor = self.select_anchor.map(|pos| self.clamp_pos(pos, last));
        let head = self.select_head.map(|pos| self.clamp_pos(pos, last));
        self.select_anchor = anchor;
        self.select_head = head;
    }

    fn clamp_pos(&self, pos: SelPos, last: usize) -> SelPos {
        let line = pos.line.min(last);
        let width = self.line_at(line).map_or(0, line_display_width);
        SelPos {
            line,
            col: pos.col.min(width),
        }
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
        let height = self.area.height.max(1);
        let rel_y = if row <= self.area.y {
            0
        } else {
            usize::from(row.saturating_sub(self.area.y)).min(usize::from(height - 1))
        };
        let raw_line = match self.sticky_prompt {
            Some((start, sticky_height)) if rel_y < usize::from(sticky_height) => {
                start.saturating_add(rel_y)
            }
            Some((_, sticky_height)) => self
                .render_start
                .saturating_add(rel_y.saturating_sub(usize::from(sticky_height))),
            None => self.current_top().saturating_add(rel_y),
        };
        let last = total - 1;
        let line = raw_line.min(last);
        let width = self.line_at(line).map_or(0, line_display_width);
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

    fn active_prompt_range(&self) -> Option<(usize, usize)> {
        let active = self.active_prompt?;
        let width = self.width();
        if width == 0 || active >= self.rows.len() {
            return None;
        }
        let mut lines = Vec::new();
        for (idx, row) in self.rows.iter().enumerate() {
            let start = lines.len();
            let live_rows = self.live_body_rows(idx);
            Self::wrap_row_into(&mut lines, row, width, live_rows);
            if idx == active {
                let first_content = lines[start..]
                    .iter()
                    .position(|line| line_display_width(line) > 0)
                    .map_or(start, |offset| start + offset);
                return Some((first_content, lines.len()));
            }
        }
        None
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

    fn collapsible_header_at(&self, column: u16, row: u16) -> Option<(usize, Header)> {
        let line = self.pos_at(column, row).line;
        let idx = self
            .rows
            .iter()
            .position(|r| r.headers.iter().any(|header| header.line == line))?;
        let header = self.rows[idx]
            .headers
            .iter()
            .find(|header| header.line == line)?
            .clone();
        Some((idx, header))
    }

    fn update_hover(&mut self, in_area: bool, column: u16, row: u16) -> bool {
        let hovered = if in_area {
            self.collapsible_header_at(column, row)
                .map(|(_, header)| header)
        } else {
            None
        };
        let changed = self.hovered_collapsible != hovered;
        self.hovered_collapsible = hovered;
        changed
    }

    fn toggle_collapsible(&mut self, row: usize, header: &Header) {
        let Some(mut target) = self.rows.get_mut(row).and_then(|r| r.collapsible.as_mut()) else {
            return;
        };
        for idx in &header.path {
            let Some(nested) = target.item_mut(*idx) else {
                return;
            };
            target = nested;
        }
        target.toggle();
        let start = self.current_top();
        self.rewrap_all();
        // Pin the clicked header to its screen row, so its body grows
        // downward instead of scrolling the header out of view.
        self.top = Some(start);
    }

    /// Settles the live thinking row on the label, for reasoning the agent
    /// never reported a span for (a cancelled or interrupted window).
    fn stop_live_thinking(&mut self) {
        self.retire_thinking(THOUGHT_LABEL.to_string());
    }

    /// The agent owns the thinking clock; the transcript only renders the
    /// duration it reports. Reporting retires the row so nothing settles it
    /// back onto the bare label.
    fn report_thinking_done(&mut self, duration_ms: u64) {
        self.retire_thinking(format_thought(Some(duration_ms)));
    }

    /// Streaming is over for the row, so it collapses: its windowed body stops
    /// here and an expanded row would blank the screen until the next row.
    fn retire_thinking(&mut self, title: String) {
        if let Some(row) = self.thinking_row.take() {
            self.rows[row].text = title;
            if let Some(collapsible) = self.rows[row].collapsible.as_mut() {
                collapsible.collapse();
            }
            self.rewrap_all();
        }
    }

    fn live_thinking_header(&self) -> Option<usize> {
        let row = self.thinking_row?;
        (self.rows[row].text == THINKING_LABEL)
            .then(|| self.rows[row].headers.first().map(|header| header.line))
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
                let page = u16::try_from(self.height().max(1)).unwrap_or(u16::MAX);
                self.scroll_up(page);
                KeyResult::Handled
            }
            KeyCode::PageDown => {
                let page = u16::try_from(self.height().max(1)).unwrap_or(u16::MAX);
                self.scroll_down(page);
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
                let double = header
                    .as_ref()
                    .filter(|(_, hit)| {
                        self.last_collapsible_click
                            .as_ref()
                            .is_some_and(|(last, at)| {
                                last.line == hit.line && at.elapsed() <= DOUBLE_CLICK_TIMEOUT
                            })
                    })
                    .map(|(row, hit)| (*row, hit.clone()));
                if let Some((row, hit)) = double {
                    self.last_collapsible_click = None;
                    self.toggle_collapsible(row, &hit);
                    self.clear_selection();
                    return KeyResult::Handled;
                }
                self.last_collapsible_click = header.map(|(_, hit)| (hit, Instant::now()));
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
            MouseEventKind::Up(MouseButton::Left) if in_area || self.dragging => {
                if self.dragging {
                    self.update_selection(mouse.column, mouse.row);
                }
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
                    self.push_thinking(THINKING_LABEL, text);
                }
                AgentEvent::Stream(StreamEvent::ThinkingDone { duration_ms }) => {
                    self.report_thinking_done(*duration_ms);
                }
                AgentEvent::Stream(StreamEvent::TextDelta { text }) => {
                    self.close_tool_burst();
                    self.stop_live_thinking();
                    self.push_stream(LineKind::Text, text);
                }
                AgentEvent::Tool(ToolEvent::ApprovalRequested { view, .. }) => {
                    self.stop_live_thinking();
                    self.flush_streaming();
                    self.push_row(
                        LineKind::System,
                        &format!("approval required: {}", view.summary),
                    );
                }
                AgentEvent::Tool(ToolEvent::Started { call_id, view, .. }) => {
                    self.stop_live_thinking();
                    self.flush_streaming();
                    self.note_tool_start(&call_id.0.to_string(), view);
                }
                AgentEvent::Tool(ToolEvent::Finished { call_id, result }) => {
                    let (ok, output) = match result {
                        ToolResult::Success { output } => (true, output.as_str()),
                        ToolResult::Failed { output, error } => {
                            (false, output.as_deref().unwrap_or(error))
                        }
                        ToolResult::Rejected { reason } => (false, reason.as_str()),
                        ToolResult::Cancelled => (false, "cancelled"),
                    };
                    self.note_tool_end(&call_id.0.to_string(), ok, output);
                }
                AgentEvent::Tool(ToolEvent::OutputDelta { .. })
                | AgentEvent::Turn(TurnEvent::Started)
                | AgentEvent::Usage { .. }
                | AgentEvent::TodosChanged { .. } => {}
                AgentEvent::Turn(TurnEvent::LoopLimitReached { max_iters, .. }) => {
                    self.stop_live_thinking();
                    self.flush_streaming();
                    self.push_row(
                        LineKind::System,
                        &format!("{LOOP_LIMIT_REACHED} ({max_iters} iterations)"),
                    );
                }
                AgentEvent::Turn(TurnEvent::Completed { duration_ms, .. }) => {
                    self.finish_response();
                    self.push_elapsed(*duration_ms);
                }
                AgentEvent::Turn(TurnEvent::Cancelled { duration_ms, .. }) => {
                    self.close_tool_burst();
                    self.stop_live_thinking();
                    if !self.streaming.is_empty() {
                        let (kind, partial) = self.take_stream();
                        if !partial.is_empty() {
                            self.append_row(kind, format!("{partial}…"), None);
                        }
                    }
                    self.push_row(LineKind::System, "cancelled");
                    self.finish_response();
                    self.push_elapsed(*duration_ms);
                }
                AgentEvent::Turn(TurnEvent::Failed {
                    error, duration_ms, ..
                }) => {
                    self.close_tool_burst();
                    self.stop_live_thinking();
                    self.flush_streaming();
                    self.push_row(LineKind::Error, &error.message);
                    self.finish_response();
                    self.push_elapsed(*duration_ms);
                }
            },
            AppEventKind::Shell(ev) => match ev {
                ShellEvent::Started { .. } => {}
                ShellEvent::Finished {
                    output, exit_code, ..
                } => {
                    self.push_shell_output(output, *exit_code == 0);
                    self.active_prompt = None;
                }
                ShellEvent::Failed { error, output, .. } => {
                    let body = if output.is_empty() { error } else { output };
                    self.push_shell_output(body, false);
                    self.active_prompt = None;
                }
            },
            AppEventKind::Compaction(ev) => {
                if matches!(ev, oven_app::CompactionEvent::Completed { .. }) {
                    self.push_row(LineKind::System, "context compacted");
                    self.push_separator();
                }
            }
            AppEventKind::StateChanged(_)
            | AppEventKind::Exited
            | AppEventKind::Notification { .. } => {}
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

        self.sticky_prompt = None;
        let mut content_area = area;
        if self.top.is_none()
            && let Some((prompt_start, prompt_end)) = self.active_prompt_range()
        {
            let prompt_lines = &self.wrapped[prompt_start..prompt_end];
            let height = prompt_lines
                .len()
                .min(MAX_STICKY_PROMPT_ROWS)
                .min(area.height as usize);
            if height > 0 {
                let height = u16::try_from(height).unwrap_or(area.height);
                let prompt_area = Rect::new(area.x, area.y, area.width, height);
                paint_visible(f, prompt_area, prompt_lines[..usize::from(height)].to_vec());
                content_area.y = content_area.y.saturating_add(height);
                content_area.height = content_area.height.saturating_sub(height);
                self.sticky_prompt = Some((prompt_start, height));
            }
        }

        let height = content_area.height as usize;
        let total = self.total_lines();
        let max_top = total.saturating_sub(height);
        if let Some(top) = self.top {
            let top = top.min(max_top);
            self.top = (top.saturating_add(height) < total).then_some(top);
        }
        let mut start = self.top.unwrap_or(max_top);
        if let Some((_, prompt_end)) = self.active_prompt_range()
            && self.sticky_prompt.is_some()
        {
            start = start.max(prompt_end);
        }
        self.render_start = start;
        let end = start.saturating_add(height).min(total);
        let mut visible = collect_lines(&self.wrapped, &self.wrapped_stream, start, end);
        if let Some(header) = self.live_thinking_header()
            && header >= start
            && let Some(line) = visible.get_mut(header - start)
        {
            *line = apply_thinking_shimmer(line, thinking_phase());
        }
        if let Some(header) = &self.hovered_collapsible
            && header.line >= start
            && let Some(line) = visible.get_mut(header.line - start)
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
        paint_visible(f, content_area, visible);
    }
}

#[cfg(test)]
fn timed_messages(messages: &[Message]) -> Vec<(Arc<Message>, u64, Option<u64>)> {
    messages
        .iter()
        .cloned()
        .map(|message| (Arc::new(message), 0, None))
        .collect()
}

fn result_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wall time a persisted turn took; `0` when the session predates timestamps.
fn turn_elapsed(started_at: Option<u64>, ended_at: u64) -> u64 {
    started_at
        .map(|start| ended_at.saturating_sub(start))
        .unwrap_or(0)
}

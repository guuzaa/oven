use std::sync::Arc;
use std::time::{Duration, Instant};

use super::activity::Activity;
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

use crate::core::component::{Action, Component, KeyResult, State};
use crate::core::theme;
use crate::platform::clipboard;

use super::kinds::{Header, LineKind, Row};
use super::selection::{SelPos, extract_line_range, highlight_line};
use super::wrap::{
    MAX_LIVE_BODY_ROWS, MAX_SHELL_DISPLAY_LINES, RESULT_LABEL, THINKING_LABEL, THOUGHT_LABEL,
    apply_hover, apply_shimmer, collect_lines, format_elapsed, format_lines, format_thought,
    line_display_width, paint_visible, shimmer_phase, sticky_prompt_lines, tail_lines,
    trim_message, wrap_collapsible_into, wrap_line_into, wrap_row_into,
};

const MOUSE_SCROLL_STEP: u16 = 3;
const STREAM_CARET: &str = "▊";
const CARET_FRAMES: u64 = 5;
const DOUBLE_CLICK_TIMEOUT: Duration = Duration::from_millis(500);
/// Rows the prompt pinned at the top of the transcript area may occupy, frame
/// included, so a long prompt cannot take the whole viewport.
pub(super) const MAX_STICKY_PROMPT_ROWS: usize = 8;
const NO_OUTPUT: &str = "(no output)";
pub(super) const LOOP_LIMIT_REACHED: &str = "agent loop limit reached";

pub struct Transcript {
    pub(super) rows: Vec<Row>,
    pub(super) wrapped: Vec<Line<'static>>,
    /// Wrapped-line offset where each row starts, parallel to `rows`.
    row_offsets: Vec<usize>,
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
    /// Thinking and tool calls since the last visible text. One burst inside
    /// it sums tool counts across steps.
    activity: Activity,
    activity_row: Option<usize>,
    /// Wrapped-line start and height of the prompt pinned at the top of the
    /// last draw. Scroll math uses it so a wheel step moves the body, not
    /// the header.
    sticky_prompt: Option<(usize, u16)>,
    render_start: usize,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            wrapped: Vec::new(),
            row_offsets: Vec::new(),
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
            activity: Activity::default(),
            activity_row: None,
            sticky_prompt: None,
            render_start: 0,
        }
    }

    /// Appends a row the user submitted: their prompt or a shell command.
    pub(super) fn push_prompt(&mut self, kind: LineKind, text: &str) {
        self.stop_live_thinking();
        self.seal_activity();
        self.push_row(kind, text);
    }

    /// Starts a turn at the tail by appending its prompt before any response
    /// events can arrive.
    pub(crate) fn start_user_turn(&mut self, text: &str) {
        self.start_turn(LineKind::User, text);
    }

    /// Starts a local shell turn at the tail.
    pub(crate) fn start_shell_turn(&mut self, command: &str) {
        self.start_turn(LineKind::Shell, command);
    }

    fn start_turn(&mut self, kind: LineKind, text: &str) {
        self.push_prompt(kind, text);
        self.top = None;
    }

    pub fn push_shell_output(&mut self, output: &str, ok: bool) {
        self.seal_activity();
        let body = trim_message(output);
        self.push_row(LineKind::ShellResult(ok), result_body(&body));
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

    /// Closes the current response the way a completed turn does. The prompt
    /// row stays where it is, so the turn keeps its header after it ends.
    pub(crate) fn finish_response(&mut self) {
        self.stop_live_thinking();
        self.flush_streaming();
        self.seal_activity();
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
                                self.seal_activity();
                                if let Some(sh) = LocalShell::try_parse(text) {
                                    self.push_prompt(LineKind::Shell, &sh.command);
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
                                self.push_thinking(&format_thought(*thinking_ms), thinking, false);
                                emitted = true;
                            }
                            ContentBlock::Text { text } => {
                                let body = trim_message(text);
                                if !body.is_empty() {
                                    self.stop_live_thinking();
                                    self.seal_activity();
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
        self.seal_activity();
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

    pub(crate) fn scroll_lines(&mut self, up: bool, n: u16) {
        match up {
            true => self.scroll_up(n),
            false => self.scroll_down(n),
        }
    }

    pub(super) fn scroll_up(&mut self, n: u16) {
        let mut origin = self.body_origin();
        for _ in 0..n {
            if origin == 0 {
                break;
            }
            origin -= 1;
            origin = self.skip_prompt_up(origin);
        }
        self.anchor_scroll(origin);
    }

    pub(super) fn scroll_down(&mut self, n: u16) {
        let tail = self.body_start(None);
        let mut origin = self.body_origin();
        for _ in 0..n {
            if origin >= tail {
                origin = tail;
                break;
            }
            origin += 1;
            origin = self.skip_prompt_down(origin, tail);
        }
        self.anchor_scroll(origin);
    }

    /// Anchors `origin` only when it draws a body line above the tail. Landing
    /// on the tail, or inside a prompt the tail already pins, keeps following
    /// new rows.
    fn anchor_scroll(&mut self, origin: usize) {
        self.top = (self.body_start(Some(origin)) < self.body_start(None)).then_some(origin);
    }

    /// First wrapped body line `top` draws. `None` follows the tail.
    ///
    /// A short turn draws that line at the prompt's end, past the index that
    /// would fill the viewport from the last line. The lines between the two
    /// are above the viewport, so a scroll that stops there is not the tail.
    fn body_start(&self, top: Option<usize>) -> usize {
        let total = self.total_lines();
        if total == 0 {
            return 0;
        }
        let prompt = self.prompt_for_focus(top, total);
        let height = Self::content_rows(self.area.height, prompt);
        let max_top = total.saturating_sub(height);
        let mut start = top.unwrap_or(max_top);
        if let Some((_, end)) = prompt
            && start < end
        {
            start = end;
        }
        start
    }

    /// First body line under the pinned prompt. Following the tail uses the
    /// origin of the last draw, so a scroll step moves from what is on screen.
    fn body_origin(&self) -> usize {
        if let Some(top) = self.top {
            return top;
        }
        if self.sticky_prompt.is_some() {
            return self.render_start;
        }
        self.total_lines().saturating_sub(self.height().max(1))
    }

    /// A prompt is a header, so scrolling up through it lands on the previous
    /// turn instead of walking the frame line by line.
    fn skip_prompt_up(&self, mut origin: usize) -> usize {
        while let Some((start, _)) = self.prompt_bounds_containing(origin) {
            if start == 0 {
                return 0;
            }
            origin = start - 1;
        }
        origin
    }

    fn skip_prompt_down(&self, mut origin: usize, max_top: usize) -> usize {
        while let Some((_, end)) = self.prompt_bounds_containing(origin) {
            if end >= max_top {
                return max_top;
            }
            origin = end;
        }
        origin
    }

    fn note_tool_start(&mut self, call_id: &str, view: &ToolView) {
        self.stop_live_thinking();
        if !view.collapse {
            let kind = if view.detail.is_some() {
                LineKind::Diff
            } else {
                LineKind::Tool
            };
            let body = view.detail.clone().unwrap_or_default();
            self.activity
                .start_standalone(call_id.to_string(), kind, view.summary.clone(), body);
        } else {
            self.activity
                .start_call(call_id.to_string(), &view.summary, view.detail.as_deref());
        }
        self.sync_activity();
    }

    fn note_tool_end(&mut self, call_id: &str, ok: bool, output: &str, detail: Option<&str>) {
        let landed = detail.filter(|text| !text.is_empty());
        if self
            .activity
            .finish_call(call_id, landed, !ok, (!ok).then_some(output))
        {
            if !ok || landed.is_some() {
                self.sync_activity();
            }
            return;
        }
        let shown = standalone_output(ok, output);
        if self
            .activity
            .finish_standalone(call_id, ok, shown.as_deref())
        {
            self.sync_activity();
        }
    }

    fn note_seed_result(&mut self, tool_use_id: &str, is_error: bool, content: &[ContentBlock]) {
        let output = result_text(content);
        let error = is_error.then_some(output.as_str());
        if self
            .activity
            .finish_call(tool_use_id, None, is_error, error)
        {
            if is_error {
                self.sync_activity();
            }
            return;
        }
        let shown = standalone_output(!is_error, &output);
        if self
            .activity
            .finish_standalone(tool_use_id, !is_error, shown.as_deref())
        {
            self.sync_activity();
        }
    }

    fn sync_activity(&mut self) {
        if self.activity.is_empty() {
            return;
        }
        if self.activity_row.is_none() {
            self.push_row_with_detail(
                LineKind::Activity,
                String::new(),
                Some(Collapsible::from_sections(Vec::new()).collapsed()),
            );
            self.activity_row = Some(self.rows.len() - 1);
        }
        self.paint_activity();
    }

    fn paint_activity(&mut self) {
        let Some(row) = self.activity_row else {
            return;
        };
        if let Some(body) = self.rows[row].collapsible.as_ref() {
            self.activity.absorb(body.sections());
        }
        self.activity.apply_pending_fold();
        let Some(open) = self.rows[row]
            .collapsible
            .as_ref()
            .map(Collapsible::open_state)
        else {
            return;
        };
        let (title, sections) = self.activity.project();
        let body = Collapsible::from_sections(sections).with_open_state(open);
        self.rows[row].text = title;
        self.rows[row].collapsible = Some(body);
        self.rewrap_row(row);
    }

    /// Stops appending to the open activity. The row stays, folded unless the
    /// user pinned it open.
    fn seal_activity(&mut self) {
        self.activity = Activity::default();
        let Some(row) = self.activity_row.take() else {
            return;
        };
        if let Some(body) = self.rows[row].collapsible.as_mut() {
            body.collapse_tree();
        }
        self.rewrap_row(row);
    }

    pub(super) fn push_row(&mut self, kind: LineKind, text: &str) {
        let (text, collapsible) = match kind {
            LineKind::ToolResult(_) => (RESULT_LABEL.to_string(), Some(Collapsible::new(text))),
            LineKind::ShellResult(_) => (tail_lines(text, MAX_SHELL_DISPLAY_LINES), None),
            _ => (text.to_string(), None),
        };
        self.push_row_with_detail(kind, text, collapsible);
    }

    /// `live` is a thinking window still receiving deltas. Seeded thoughts are
    /// already finished, so they must not be retitled when the next tool starts.
    fn push_thinking(&mut self, title: &str, text: &str, live: bool) {
        if self.activity.push_thinking(title, text, live) {
            self.patch_live_thought(title, text);
            return;
        }
        self.sync_activity();
    }

    /// A delta on the thought already at the end of the row. The tool sections
    /// stay as they are; only that thought's text is extended.
    fn patch_live_thought(&mut self, title: &str, delta: &str) {
        let Some(row) = self.activity_row else {
            self.sync_activity();
            return;
        };
        if self.activity.is_flat_thinking() {
            self.rows[row].text = title.to_string();
            if let Some(body) = self.rows[row].collapsible.as_mut() {
                body.append(delta);
            }
            self.rewrap_row(row);
            return;
        }
        let Some(idx) = self.activity.live_thinking_index() else {
            self.sync_activity();
            return;
        };
        let patched = self.rows[row]
            .collapsible
            .as_mut()
            .is_some_and(|body| body.append_item_text(idx, title, delta));
        if patched {
            self.rewrap_row(row);
        } else {
            self.sync_activity();
        }
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

    fn append_row(&mut self, kind: LineKind, text: String, collapsible: Option<Collapsible>) {
        self.rows.push(Row {
            kind,
            text,
            collapsible,
            headers: Vec::new(),
        });
        self.row_offsets.push(self.wrapped.len());
        self.wrap_row(self.rows.len() - 1);
    }

    /// Closes every body a new row opens, and rewraps just those rows: a row
    /// rewrapped in place shifts the ones after it as a block, so walking the
    /// rows forwards keeps every span valid for the next one.
    fn collapse_open(&mut self) {
        for idx in 0..self.rows.len() {
            let closes = self.rows[idx]
                .collapsible
                .as_mut()
                .is_some_and(Collapsible::collapse);
            if closes {
                self.rewrap_row(idx);
            }
        }
    }

    /// Wraps one row into `out`, asking for `separator`: the blank line that
    /// keeps rows apart, which every row after the first one leads with.
    fn wrap_row_into(
        out: &mut Vec<Line<'static>>,
        row: &Row,
        width: usize,
        live_rows: Option<usize>,
        separator: bool,
    ) -> Vec<Header> {
        if let Some(collapsible) = &row.collapsible {
            wrap_collapsible_into(
                out,
                row.kind,
                &row.text,
                collapsible,
                width,
                live_rows,
                separator,
            )
        } else {
            wrap_row_into(out, row.kind, &row.text, width, separator);
            Vec::new()
        }
    }

    /// Settles the response and stamps how long the turn took.
    fn end_turn(&mut self, duration_ms: u64) {
        self.finish_response();
        self.push_elapsed(duration_ms);
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
        let headers = if width == 0 {
            Vec::new()
        } else {
            let separator = !self.wrapped.is_empty();
            let live_rows = self.live_body_rows(idx);
            Self::wrap_row_into(
                &mut self.wrapped,
                &self.rows[idx],
                width,
                live_rows,
                separator,
            )
        };
        self.rows[idx].headers = headers;
    }

    /// Rewraps one row where it stands: the wrapped lines above it stay as they
    /// are and the rows after it move as a block, so an event that touches a
    /// single row — a retitled burst, a thinking delta — pays for that row
    /// instead of the whole conversation.
    fn rewrap_row(&mut self, idx: usize) {
        let start = self.row_offsets[idx];
        let end = self
            .row_offsets
            .get(idx + 1)
            .copied()
            .unwrap_or(self.wrapped.len());
        let mut lines = Vec::new();
        let mut headers = match self.width() {
            0 => Vec::new(),
            width => {
                let separator = start > 0;
                let live_rows = self.live_body_rows(idx);
                Self::wrap_row_into(&mut lines, &self.rows[idx], width, live_rows, separator)
            }
        };
        // Wrapped into a buffer of its own, so a marker's line comes back
        // relative to the row and only means anything once `start` is added.
        for header in &mut headers {
            header.line += start;
        }
        self.rows[idx].headers = headers;
        let delta = shift_of(end - start, lines.len());
        self.wrapped.splice(start..end, lines);
        if delta != 0 {
            for offset in &mut self.row_offsets[idx + 1..] {
                *offset = offset.saturating_add_signed(delta);
            }
            self.shift_selection(end, delta);
        }
    }

    /// A body that is still growing — live thinking or tool calls still in
    /// flight — renders only its newest screen rows, so it cannot scroll the
    /// view up.
    fn live_body_rows(&self, idx: usize) -> Option<usize> {
        let live = self.activity_row == Some(idx)
            && (self.activity.thinking_live() || self.activity.tools_running());
        live.then_some(MAX_LIVE_BODY_ROWS)
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
        self.row_offsets.clear();
        for idx in 0..self.rows.len() {
            self.row_offsets.push(self.wrapped.len());
            self.wrap_row(idx);
        }
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

    /// Keeps a drag alive across a row's rewrap: lines at or below the row
    /// moved with it, the ones above it stayed put, so both ends follow.
    fn shift_selection(&mut self, edge: usize, delta: isize) {
        self.select_anchor = self.select_anchor.map(|pos| shift_pos(pos, edge, delta));
        self.select_head = self.select_head.map(|pos| shift_pos(pos, edge, delta));
        self.reanchor_selection();
    }

    fn line_width_at(&self, line: usize) -> usize {
        self.line_at(line).map_or(0, line_display_width)
    }

    fn clamp_pos(&self, pos: SelPos, last: usize) -> SelPos {
        let line = pos.line.min(last);
        let width = self.line_width_at(line);
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
            Some(text) if clipboard::copy(&text) => true,
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
        let width = self.line_width_at(line);
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

    /// Which prompt to pin, and the first body line under it.
    ///
    /// The prompt is the user or shell row of the turn that contains the
    /// viewport's focus line. Following the tail focuses the last line, so
    /// the latest turn stays pinned after it ends. Scrolling into an earlier
    /// turn pins that turn instead. Body lines start after the pinned prompt
    /// so the question is not drawn twice.
    fn layout_viewport(&mut self, area_height: u16) -> (Option<(usize, usize)>, usize) {
        let total = self.total_lines();
        if total == 0 {
            return (None, 0);
        }
        let mut prompt = None;
        for _ in 0..2 {
            let next = self.prompt_for_focus(self.top, total);
            let before = self.top;
            self.clamp_top(total, Self::content_rows(area_height, next));
            prompt = self.prompt_for_focus(self.top, total);
            if prompt == next && self.top == before {
                break;
            }
        }
        (prompt, self.body_start(self.top))
    }

    fn content_rows(area_height: u16, prompt: Option<(usize, usize)>) -> usize {
        let sticky_h = prompt
            .map(|(start, end)| (end - start).min(MAX_STICKY_PROMPT_ROWS))
            .unwrap_or(0)
            .min(usize::from(area_height));
        usize::from(area_height).saturating_sub(sticky_h)
    }

    fn prompt_for_focus(&self, top: Option<usize>, total: usize) -> Option<(usize, usize)> {
        let focus = top.unwrap_or(total.saturating_sub(1));
        self.sticky_prompt_range(focus)
    }

    fn clamp_top(&mut self, total: usize, height: usize) {
        if height == 0 || total == 0 {
            return;
        }
        if self
            .top
            .is_some_and(|top| self.body_start(Some(top)) >= self.body_start(None))
        {
            self.top = None;
        }
    }

    /// Prompt of the turn that contains `line`: the latest user or shell row
    /// whose wrapped lines start at or before it.
    fn sticky_prompt_range(&self, line: usize) -> Option<(usize, usize)> {
        let mut found = None;
        for (idx, row) in self.rows.iter().enumerate() {
            if !row.kind.is_prompt() {
                continue;
            }
            let Some(range) = self.row_wrapped_range(idx) else {
                continue;
            };
            if range.0 > line {
                break;
            }
            found = Some(range);
        }
        found
    }

    /// Raw wrapped span of a prompt, including its frame, so scrolling can
    /// step over the whole header.
    fn prompt_bounds_containing(&self, line: usize) -> Option<(usize, usize)> {
        for (idx, row) in self.rows.iter().enumerate() {
            if !row.kind.is_prompt() {
                continue;
            }
            let start = *self.row_offsets.get(idx)?;
            let end = self
                .row_offsets
                .get(idx + 1)
                .copied()
                .unwrap_or(self.wrapped.len());
            if start <= line && line < end {
                return Some((start, end));
            }
        }
        None
    }

    /// Wrapped-line range of one row, from its first non-empty line to the
    /// start of the next row.
    fn row_wrapped_range(&self, row: usize) -> Option<(usize, usize)> {
        let start = *self.row_offsets.get(row)?;
        let end = self
            .row_offsets
            .get(row + 1)
            .copied()
            .unwrap_or(self.wrapped.len());
        if start >= end {
            return None;
        }
        let first_content = self.wrapped[start..end]
            .iter()
            .position(|line| line_display_width(line) > 0)
            .map_or(start, |offset| start + offset);
        Some((first_content, end))
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
        // The first body line the last layout would draw, so the toggled
        // block keeps the screen row it was clicked on.
        let (_, start) = self.layout_viewport(self.area.height);
        self.rewrap_row(row);
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

    /// Streaming is over for the block, so it collapses: its windowed body stops
    /// here and an expanded block would blank the screen until the next row.
    fn retire_thinking(&mut self, title: String) {
        if !self.activity.retire_thinking(&title) {
            return;
        }
        let fold = self.activity.is_flat_thinking();
        self.sync_activity();
        if fold
            && let Some(row) = self.activity_row
            && let Some(body) = self.rows[row].collapsible.as_mut()
        {
            body.collapse_tree();
            self.rewrap_row(row);
        }
    }

    /// Header lines that should shimmer: the activity header while it is
    /// folded, or the live thought once the user opens the timeline.
    fn shimmer_lines(&self) -> Vec<usize> {
        let Some(row) = self.activity_row else {
            return Vec::new();
        };
        if self.activity.thinking_live() || self.activity.tools_running() {
            self.activity_shimmer(row)
        } else {
            Vec::new()
        }
    }

    fn activity_shimmer(&self, row: usize) -> Vec<usize> {
        let expanded = self.rows[row]
            .collapsible
            .as_ref()
            .is_some_and(Collapsible::is_expanded);
        if expanded && let Some(idx) = self.activity.live_thinking_index() {
            return self.header_line(row, &[idx]).into_iter().collect();
        }
        self.header_line(row, &[]).into_iter().collect()
    }

    fn header_line(&self, row: usize, path: &[usize]) -> Option<usize> {
        self.rows.get(row).and_then(|row| {
            row.headers
                .iter()
                .find(|header| header.path == path)
                .map(|header| header.line)
        })
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
                    self.push_thinking(THINKING_LABEL, text, true);
                }
                AgentEvent::Stream(StreamEvent::ThinkingDone { duration_ms }) => {
                    self.report_thinking_done(*duration_ms);
                }
                AgentEvent::Stream(StreamEvent::TextDelta { text }) => {
                    self.stop_live_thinking();
                    self.seal_activity();
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
                AgentEvent::Tool(ToolEvent::Finished {
                    call_id,
                    result,
                    detail,
                }) => {
                    let (ok, output) = match result {
                        ToolResult::Success { output } => (true, output.as_str()),
                        ToolResult::Failed { output, error } => {
                            (false, output.as_deref().unwrap_or(error))
                        }
                        ToolResult::Rejected { reason } => (false, reason.as_str()),
                        ToolResult::Cancelled => (false, "cancelled"),
                    };
                    self.note_tool_end(&call_id.0.to_string(), ok, output, detail.as_deref());
                }
                AgentEvent::Tool(ToolEvent::OutputDelta { .. })
                | AgentEvent::Tool(ToolEvent::QuestionAsked { .. })
                | AgentEvent::Turn(TurnEvent::Started)
                | AgentEvent::Turn(TurnEvent::StepStarted { .. })
                | AgentEvent::Turn(TurnEvent::StepFinished { .. })
                | AgentEvent::Usage { .. }
                | AgentEvent::TodosChanged { .. } => {}
                AgentEvent::Turn(TurnEvent::UserAppended { text }) => {
                    self.flush_streaming();
                    self.push_prompt(LineKind::User, text);
                }
                AgentEvent::Turn(TurnEvent::LoopLimitReached { max_iters, .. }) => {
                    self.stop_live_thinking();
                    self.flush_streaming();
                    self.push_row(
                        LineKind::System,
                        &format!("{LOOP_LIMIT_REACHED} ({max_iters} iterations)"),
                    );
                }
                AgentEvent::Turn(TurnEvent::Completed { duration_ms, .. }) => {
                    self.end_turn(*duration_ms);
                }
                AgentEvent::Turn(TurnEvent::Cancelled { duration_ms, .. }) => {
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
                    self.stop_live_thinking();
                    self.flush_streaming();
                    self.push_row(LineKind::Error, &error.message);
                    self.end_turn(*duration_ms);
                }
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
            AppEventKind::HistoryChanged { .. }
            | AppEventKind::Exited
            | AppEventKind::Subagent(_)
            | AppEventKind::Notification { .. }
            | AppEventKind::RequestResolved { .. } => {}
            AppEventKind::Error { message } => {
                self.stop_live_thinking();
                self.flush_streaming();
                self.seal_activity();
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

        let (prompt_range, content_start) = self.layout_viewport(area.height);
        self.sticky_prompt = None;
        let mut content_area = area;
        if let Some((prompt_start, prompt_end)) = prompt_range {
            let prompt_lines = &self.wrapped[prompt_start..prompt_end];
            let height = prompt_lines
                .len()
                .min(MAX_STICKY_PROMPT_ROWS)
                .min(area.height as usize);
            if height > 0 {
                let height = u16::try_from(height).unwrap_or(area.height);
                let prompt_area = Rect::new(area.x, area.y, area.width, height);
                let lines = sticky_prompt_lines(prompt_lines, usize::from(height));
                paint_visible(f, prompt_area, lines);
                content_area.y = content_area.y.saturating_add(height);
                content_area.height = content_area.height.saturating_sub(height);
                self.sticky_prompt = Some((prompt_start, height));
            }
        }

        let height = content_area.height as usize;
        let total = self.total_lines();
        let start = content_start;
        self.render_start = start;
        let end = start.saturating_add(height).min(total);
        let mut visible = collect_lines(&self.wrapped, &self.wrapped_stream, start, end);
        let phase = shimmer_phase();
        for header in self.shimmer_lines() {
            if header >= start
                && let Some(line) = visible.get_mut(header - start)
            {
                *line = apply_shimmer(line, phase);
            }
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

/// A result body, or the placeholder the transcript shows for no output.
fn result_body(body: &str) -> &str {
    if body.is_empty() { NO_OUTPUT } else { body }
}

/// What a standalone call should append. A success with no output adds nothing.
fn standalone_output(ok: bool, output: &str) -> Option<String> {
    let body = trim_message(output);
    if ok && body.is_empty() {
        None
    } else {
        Some(result_body(&body).to_string())
    }
}

/// How far a wrapped line count moved, signed: lengths are bounded by memory,
/// so the impossible case saturates rather than wrapping around.
fn shift_of(before: usize, after: usize) -> isize {
    let before = isize::try_from(before).unwrap_or(isize::MAX);
    let after = isize::try_from(after).unwrap_or(isize::MAX);
    after.saturating_sub(before)
}

/// A selection end that moved with the row it sits in; the ones above the row
/// kept their lines.
fn shift_pos(pos: SelPos, edge: usize, delta: isize) -> SelPos {
    if pos.line < edge {
        return pos;
    }
    SelPos {
        line: pos.line.saturating_add_signed(delta),
        col: pos.col,
    }
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

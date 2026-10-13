//! Assistant prose is redrawn from the whole message on every delta.
//!
//! The screen may hide emphasis markers and draw `•` / `│` in place of list
//! and quote markers. Selection copies the source bytes those columns came
//! from, so a soft wrap does not insert a newline and the clipboard does not
//! contain the display glyphs.

use std::ops::Range;
use std::sync::Arc;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::selection::extract_line_range;
use crate::core::theme;

const BULLET: char = '•';
const QUOTE_BAR: char = '│';
const FENCE_MIN: usize = 3;
const HEADING_MAX: usize = 6;
const TAB_WIDTH: usize = 4;
const TABLE_GAP: usize = 2;
const MIN_COL_WIDTH: usize = 1;

#[derive(Clone)]
struct Run {
    col: usize,
    width: usize,
    start: usize,
    end: usize,
}

pub(super) struct MdLine {
    pub spans: Vec<Span<'static>>,
    pub hang: usize,
    source: Arc<str>,
    runs: Arc<[Run]>,
    table: Option<TableMeta>,
}

#[derive(Clone)]
pub(super) struct MdCopy {
    source: Arc<str>,
    runs: Arc<[Run]>,
    origin: usize,
    body_from: usize,
    body_to: usize,
    pub break_before: bool,
}

#[derive(Clone)]
pub(super) enum LineCopy {
    Visual,
    Markdown(MdCopy),
}

impl MdLine {
    pub(super) fn copy_window(
        &self,
        origin: usize,
        body_from: usize,
        body_to: usize,
        break_before: bool,
    ) -> MdCopy {
        MdCopy {
            source: Arc::clone(&self.source),
            runs: Arc::clone(&self.runs),
            origin,
            body_from,
            body_to,
            break_before,
        }
    }
}

impl MdCopy {
    fn ranges_between(&self, from: usize, to: usize) -> Vec<Range<usize>> {
        if from >= to {
            return Vec::new();
        }
        let start = self
            .body_from
            .saturating_add(from.saturating_sub(self.origin))
            .clamp(self.body_from, self.body_to);
        let end = self
            .body_from
            .saturating_add(to.saturating_sub(self.origin))
            .clamp(start, self.body_to);
        let mut out = Vec::new();
        for run in self.runs.iter() {
            let owned = run.col >= self.body_from && run.col < self.body_to;
            let overlaps = run.col < end && run.col + run.width > start;
            if owned && overlaps && run.start < run.end {
                out.push(run.start..run.end);
            }
        }
        out
    }
}

/// Joins a drag. Soft-wrapped pieces of one source line merge by source
/// offset, so a table row wrapped inside its columns still copies in order.
pub(super) struct CopyBuf {
    out: String,
    pending: Option<PendingCopy>,
}

struct PendingCopy {
    source: Arc<str>,
    ranges: Vec<Range<usize>>,
}

impl CopyBuf {
    pub(super) fn new() -> Self {
        Self {
            out: String::new(),
            pending: None,
        }
    }

    pub(super) fn push(
        &mut self,
        copy: &LineCopy,
        line: &Line<'_>,
        from: usize,
        to: usize,
        after_start: bool,
    ) {
        match copy {
            LineCopy::Visual => {
                self.flush();
                if after_start {
                    self.out.push('\n');
                }
                self.out.push_str(&extract_line_range(line, from, to));
            }
            LineCopy::Markdown(md) => {
                let ranges = md.ranges_between(from, to);
                let newline = after_start && md.break_before;
                let same = !newline
                    && self
                        .pending
                        .as_ref()
                        .is_some_and(|pending| Arc::ptr_eq(&pending.source, &md.source));
                if same && let Some(pending) = self.pending.as_mut() {
                    pending.ranges.extend(ranges);
                    return;
                }
                self.flush();
                if newline {
                    self.out.push('\n');
                }
                self.pending = Some(PendingCopy {
                    source: Arc::clone(&md.source),
                    ranges,
                });
            }
        }
    }

    pub(super) fn finish(mut self) -> Option<String> {
        self.flush();
        if self.out.is_empty() {
            None
        } else {
            Some(self.out)
        }
    }

    fn flush(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.out
            .push_str(&emit_ranges(&pending.source, &pending.ranges));
    }
}

fn emit_ranges(source: &str, ranges: &[Range<usize>]) -> String {
    let mut spans: Vec<Range<usize>> = ranges
        .iter()
        .filter(|range| range.start < range.end)
        .cloned()
        .collect();
    spans.sort_by_key(|range| (range.start, range.end));
    let len = source.len();
    let mut out = String::new();
    let mut covered = 0;
    let mut begun = false;
    for range in spans {
        let mut start = range.start.min(len);
        let end = range.end.min(len);
        if begun && start < covered {
            start = covered;
        }
        if start >= end {
            continue;
        }
        out.push_str(&source[start..end]);
        covered = end;
        begun = true;
    }
    out
}

pub(super) fn render_markdown(text: &str) -> Vec<MdLine> {
    if text.is_empty() {
        return vec![blank_line("")];
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::with_capacity(lines.len());
    let mut fence: Option<Fence> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if fence.is_some() {
            let style = if fence.as_ref().is_some_and(|open| closes_fence(line, open)) {
                fence = None;
                theme::dim()
            } else {
                theme::code()
            };
            out.push(verbatim(line, style, leading_width(line)));
            i += 1;
            continue;
        }
        if let Some(open) = opening_fence(line) {
            fence = Some(open);
            out.push(verbatim(line, theme::dim(), leading_width(line)));
            i += 1;
            continue;
        }
        if let Some(n) = table_len(&lines[i..]) {
            out.extend(render_table(&lines[i..i + n]));
            i += n;
            continue;
        }
        out.push(render_flow(line));
        i += 1;
    }
    if out.is_empty() {
        out.push(blank_line(""));
    }
    out
}

struct Fence {
    marker: u8,
    len: usize,
}

fn opening_fence(line: &str) -> Option<Fence> {
    let rest = line.trim_start_matches([' ', '\t']);
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let marker = bytes[0];
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let len = bytes.iter().take_while(|b| **b == marker).count();
    if len < FENCE_MIN {
        return None;
    }
    let info = &rest[len..];
    if marker == b'`' && info.contains('`') {
        return None;
    }
    Some(Fence { marker, len })
}

fn closes_fence(line: &str, fence: &Fence) -> bool {
    let rest = line.trim_start_matches([' ', '\t']);
    let bytes = rest.as_bytes();
    let len = bytes.iter().take_while(|b| **b == fence.marker).count();
    len >= fence.len && rest[len..].trim().is_empty()
}

fn render_flow(line: &str) -> MdLine {
    if line.is_empty() {
        return blank_line(line);
    }
    if is_thematic_break(line) {
        return verbatim(line, theme::dim(), 0);
    }
    if let Some((level, content_at)) = atx_heading(line) {
        return render_heading(line, level, content_at);
    }
    render_marked(line)
}

fn render_heading(line: &str, level: usize, content_at: usize) -> MdLine {
    let indent_at = leading_bytes(line);
    let (shown, hidden) = strip_atx_closer(&line[content_at..]);
    let mut b = Builder::new(line);
    let mut i = 0;
    while i < indent_at {
        emit_src_char(&mut b, line, i);
        i += line[i..].chars().next().map_or(1, char::len_utf8);
    }
    if shown.is_empty() {
        while i < line.len() {
            emit_src_char(&mut b, line, i);
            i += line[i..].chars().next().map_or(1, char::len_utf8);
        }
        for span in &mut b.spans {
            span.style = theme::dim();
        }
        return b.finish(leading_width(&line[..indent_at]));
    }
    let pieces = inline_pieces(shown, theme::heading(level));
    emit_pieces(&mut b, pieces, content_at);
    // Hashes and a closing `#` sequence are not drawn; they stay on the edge runs.
    extend_first_content(&mut b, indent_at);
    if !hidden.is_empty()
        && let Some(run) = b.runs.last_mut()
    {
        run.end = line.len();
    }
    b.finish(leading_width(&line[..indent_at]))
}

fn render_marked(line: &str) -> MdLine {
    let mut b = Builder::new(line);
    let mut i = 0;
    let bytes = line.as_bytes();
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        emit_src_char(&mut b, line, i);
        i += 1;
    }
    while i < bytes.len() && bytes[i] == b'>' {
        emit_one(&mut b, QUOTE_BAR, i, i + 1, theme::dim());
        i += 1;
        if i < bytes.len() && bytes[i] == b' ' {
            emit_src_char(&mut b, line, i);
            i += 1;
        }
    }
    let quoted = b.col > 0 && line[..i].contains('>');
    if let Some(marker_end) = list_marker_end(&line[i..]) {
        emit_list_marker(&mut b, &line[i..i + marker_end], i);
        i += marker_end;
    }
    let hang = b.col;
    let content = &line[i..];
    let base = if quoted {
        Style::default().add_modifier(Modifier::ITALIC)
    } else {
        Style::default()
    };
    if !content.is_empty() {
        let pieces = inline_pieces(content, base);
        emit_pieces(&mut b, pieces, i);
    }
    b.finish(hang)
}

fn emit_list_marker(b: &mut Builder, marker: &str, src_at: usize) {
    let mut chars = marker.char_indices();
    let Some((off, first)) = chars.next() else {
        return;
    };
    let unordered = matches!(first, '-' | '*' | '+');
    let shown = if unordered { BULLET } else { first };
    let style = if unordered || first.is_ascii_digit() {
        theme::accent()
    } else {
        Style::default()
    };
    emit_one(
        b,
        shown,
        src_at + off,
        src_at + off + first.len_utf8(),
        style,
    );
    for (off, ch) in chars {
        let at = src_at + off;
        if ch == '\t' {
            emit_tab(b, at);
        } else if ch.is_ascii_digit() || ch == '.' || ch == ')' {
            emit_one(b, ch, at, at + ch.len_utf8(), theme::accent());
        } else {
            emit_one(b, ch, at, at + ch.len_utf8(), Style::default());
        }
    }
}

fn list_marker_end(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let marker = if matches!(bytes[0], b'-' | b'*' | b'+') {
        1
    } else {
        let digits = bytes
            .iter()
            .take(9)
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits == 0 || digits >= bytes.len() {
            return None;
        }
        if bytes[digits] != b'.' && bytes[digits] != b')' {
            return None;
        }
        digits + 1
    };
    if marker < bytes.len() && bytes[marker] != b' ' && bytes[marker] != b'\t' {
        return None;
    }
    let mut end = marker;
    while end < bytes.len() && (bytes[end] == b' ' || bytes[end] == b'\t') {
        end += 1;
    }
    if let Some(box_len) = task_box(&rest[end..]) {
        end += box_len;
        while end < bytes.len() && (bytes[end] == b' ' || bytes[end] == b'\t') {
            end += 1;
        }
    }
    Some(end)
}

fn task_box(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    if bytes.len() >= 3
        && bytes[0] == b'['
        && bytes[2] == b']'
        && matches!(bytes[1], b' ' | b'x' | b'X')
    {
        Some(3)
    } else {
        None
    }
}

struct Piece {
    display: String,
    origin: Range<usize>,
    range: Range<usize>,
    style: Style,
}

fn inline_pieces(content: &str, base: Style) -> Vec<Piece> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let mut style = base;
    let mut stack = Vec::new();
    let mut marks = Vec::new();
    let mut pieces = Vec::new();
    for (event, range) in Parser::new_ext(content, opts).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if let Some(kind) = inline_kind(&tag) {
                    marks.push(range.clone());
                    stack.push(style);
                    style = kind_style(base, kind);
                }
            }
            Event::End(tag) => {
                if inline_end(&tag) {
                    style = stack.pop().unwrap_or(base);
                }
            }
            Event::Text(text) => pieces.push(piece(text.into_string(), range, style)),
            Event::Code(text) => pieces.push(piece(text.into_string(), range, theme::code())),
            Event::InlineHtml(text) | Event::Html(text) => {
                pieces.push(piece(text.into_string(), range, theme::dim()));
            }
            _ => {}
        }
    }
    if pieces.is_empty() {
        if content.is_empty() {
            return pieces;
        }
        return vec![piece(content.to_string(), 0..content.len(), base)];
    }
    cover_gaps(&mut pieces, &marks, content.len());
    pieces
}

fn piece(display: String, range: Range<usize>, style: Style) -> Piece {
    Piece {
        display,
        origin: range.clone(),
        range,
        style,
    }
}

enum Kind {
    Strong,
    Em,
    Strike,
    Link,
}

fn inline_kind(tag: &Tag<'_>) -> Option<Kind> {
    match tag {
        Tag::Strong => Some(Kind::Strong),
        Tag::Emphasis => Some(Kind::Em),
        Tag::Strikethrough => Some(Kind::Strike),
        Tag::Link { .. } | Tag::Image { .. } => Some(Kind::Link),
        _ => None,
    }
}

fn inline_end(tag: &TagEnd) -> bool {
    matches!(
        tag,
        TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link | TagEnd::Image
    )
}

fn kind_style(base: Style, kind: Kind) -> Style {
    match kind {
        Kind::Strong => base.add_modifier(Modifier::BOLD),
        Kind::Em => base.add_modifier(Modifier::ITALIC),
        Kind::Strike => base.add_modifier(Modifier::CROSSED_OUT),
        Kind::Link => theme::link(),
    }
}

/// Hidden delimiters and skipped escapes stay in the source map: a closer
/// sticks to the piece it ends, everything else sticks to the piece after it.
fn cover_gaps(pieces: &mut [Piece], marks: &[Range<usize>], len: usize) {
    if pieces.is_empty() {
        return;
    }
    if pieces[0].range.start > 0 {
        pieces[0].range.start = 0;
    }
    for i in 0..pieces.len() - 1 {
        let gap_start = pieces[i].range.end;
        let gap_end = pieces[i + 1].range.start;
        if gap_start >= gap_end {
            continue;
        }
        let prev = &pieces[i].range;
        let closes = marks
            .iter()
            .any(|mark| mark.end == gap_end && mark.start <= prev.start && prev.end <= mark.end);
        if closes {
            pieces[i].range.end = gap_end;
        } else {
            pieces[i + 1].range.start = gap_start;
        }
    }
    let last = pieces.len() - 1;
    if pieces[last].range.end < len {
        pieces[last].range.end = len;
    }
}

fn emit_pieces(b: &mut Builder, pieces: Vec<Piece>, offset: usize) {
    for piece in pieces {
        let src_start = offset + piece.range.start;
        let src_end = offset + piece.range.end;
        let origin_src = &b.source[offset + piece.origin.start..offset + piece.origin.end];
        let inner = origin_src.find(&piece.display).unwrap_or(0);
        let disp_start = offset + piece.origin.start + inner;
        emit_display(
            b,
            &piece.display,
            src_start,
            src_end,
            disp_start,
            piece.style,
        );
    }
}

struct Builder {
    source: String,
    spans: Vec<Span<'static>>,
    runs: Vec<Run>,
    col: usize,
}

impl Builder {
    fn new(source: &str) -> Self {
        Self {
            source: source.to_string(),
            spans: Vec::new(),
            runs: Vec::new(),
            col: 0,
        }
    }

    fn push_span(&mut self, text: String, style: Style) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.spans.last_mut()
            && last.style == style
        {
            last.content.to_mut().push_str(&text);
            return;
        }
        let span = if style == Style::default() {
            Span::raw(text)
        } else {
            Span::styled(text, style)
        };
        self.spans.push(span);
    }

    fn finish(self, hang: usize) -> MdLine {
        let spans = if self.spans.is_empty() {
            vec![Span::raw("")]
        } else {
            self.spans
        };
        MdLine {
            spans,
            hang,
            source: Arc::from(self.source),
            runs: Arc::from(self.runs),
            table: None,
        }
    }
}

fn emit_display(
    b: &mut Builder,
    display: &str,
    src_start: usize,
    src_end: usize,
    disp_start: usize,
    style: Style,
) {
    if display.is_empty() {
        if let Some(run) = b.runs.last_mut() {
            run.end = src_end.max(run.end);
        }
        return;
    }
    let mut byte = disp_start;
    let chars: Vec<char> = display.chars().collect();
    for (i, ch) in chars.iter().enumerate() {
        let len = ch.len_utf8();
        let mut start = byte;
        let mut end = byte + len;
        if i == 0 {
            start = src_start;
        }
        if i + 1 == chars.len() {
            end = src_end;
        }
        // A zero-width char has no column; keep its bytes on the previous run.
        let w = width_of(*ch);
        if w == 0 {
            if let Some(run) = b.runs.last_mut() {
                run.end = end.max(run.end);
            }
            byte += len;
            continue;
        }
        b.runs.push(Run {
            col: b.col,
            width: w,
            start,
            end,
        });
        b.col += w;
        byte += len;
    }
    let shown = display.replace('\t', "    ");
    b.push_span(shown, style);
}

fn emit_src_char(b: &mut Builder, line: &str, byte: usize) {
    let Some(ch) = line[byte..].chars().next() else {
        return;
    };
    if ch == '\t' {
        emit_tab(b, byte);
    } else {
        emit_one(b, ch, byte, byte + ch.len_utf8(), Style::default());
    }
}

fn emit_one(b: &mut Builder, shown: char, start: usize, end: usize, style: Style) {
    let w = width_of(shown);
    if w == 0 {
        return;
    }
    b.runs.push(Run {
        col: b.col,
        width: w,
        start,
        end,
    });
    b.col += w;
    b.push_span(shown.to_string(), style);
}

fn emit_tab(b: &mut Builder, start: usize) {
    b.runs.push(Run {
        col: b.col,
        width: TAB_WIDTH,
        start,
        end: start + 1,
    });
    b.col += TAB_WIDTH;
    b.push_span("    ".to_string(), Style::default());
}

fn extend_first_content(b: &mut Builder, from: usize) {
    if let Some(run) = b.runs.iter_mut().find(|run| run.start >= from) {
        run.start = from;
    }
}

fn verbatim(line: &str, style: Style, hang: usize) -> MdLine {
    let mut b = Builder::new(line);
    let mut i = 0;
    while i < line.len() {
        emit_src_char(&mut b, line, i);
        i += line[i..].chars().next().map_or(1, char::len_utf8);
    }
    // Retint the whole line. Tabs were already expanded as their own spans.
    for span in &mut b.spans {
        span.style = style;
    }
    b.finish(hang)
}

fn blank_line(line: &str) -> MdLine {
    MdLine {
        spans: vec![Span::raw("")],
        hang: 0,
        source: Arc::from(line),
        runs: Arc::from(Vec::new()),
        table: None,
    }
}

fn atx_heading(line: &str) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let start = i;
    while i < bytes.len() && bytes[i] == b'#' {
        i += 1;
    }
    let level = i - start;
    if level == 0 || level > HEADING_MAX {
        return None;
    }
    if i < bytes.len() && bytes[i] != b' ' && bytes[i] != b'\t' {
        return None;
    }
    if i < bytes.len() {
        i += 1;
    }
    Some((level, i))
}

fn strip_atx_closer(content: &str) -> (&str, &str) {
    let trimmed = content.trim_end();
    let body = trimmed.trim_end_matches('#');
    if body.len() == trimmed.len() || !body.ends_with(' ') {
        return (content, "");
    }
    let shown = body.trim_end();
    (shown, &content[shown.len()..])
}

fn is_thematic_break(line: &str) -> bool {
    let mut marks = line.chars().filter(|c| !c.is_whitespace());
    let Some(first) = marks.next() else {
        return false;
    };
    if first != '-' && first != '*' && first != '_' {
        return false;
    }
    let mut count = 1;
    for ch in marks {
        if ch != first {
            return false;
        }
        count += 1;
    }
    count >= FENCE_MIN
}

fn table_len(lines: &[&str]) -> Option<usize> {
    if lines.len() < 2 || !is_table_row(lines[0]) || !is_table_separator(lines[1]) {
        return None;
    }
    let mut n = 2;
    while n < lines.len() && is_table_row(lines[n]) {
        n += 1;
    }
    Some(n)
}

fn is_table_row(line: &str) -> bool {
    !line.trim().is_empty() && line.contains('|')
}

fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.contains('-')
        || !trimmed.contains('|') && trimmed.chars().filter(|c| *c == '-').count() < FENCE_MIN
    {
        return false;
    }
    let mut dashes = 0;
    for ch in trimmed.chars() {
        match ch {
            '-' => dashes += 1,
            '|' | ':' | ' ' | '\t' => {}
            _ => return false,
        }
    }
    dashes >= FENCE_MIN
}

#[derive(Clone)]
struct Atom {
    text: String,
    style: Style,
    width: usize,
    start: usize,
    end: usize,
}

struct TableMeta {
    separator: bool,
    style: Style,
    indent: Vec<Atom>,
    widths: Vec<usize>,
    gaps: Vec<Vec<Atom>>,
    /// Each cell is a list of lines. A `<br>` starts a new line.
    cells: Vec<Vec<Vec<Atom>>>,
}

#[derive(Clone, Copy)]
struct CellRange {
    start: usize,
    end: usize,
}

enum Group {
    Indent(Vec<Atom>),
    Cell(Vec<Atom>),
    Gap(Vec<Atom>),
}

impl Group {
    fn atoms_mut(&mut self) -> &mut Vec<Atom> {
        match self {
            Self::Indent(atoms) | Self::Cell(atoms) | Self::Gap(atoms) => atoms,
        }
    }
}

fn render_table(lines: &[&str]) -> Vec<MdLine> {
    let cols = lines
        .iter()
        .map(|line| cell_ranges(line).len())
        .max()
        .unwrap_or(0)
        .max(1);
    let mut widths = vec![MIN_COL_WIDTH; cols];
    for (idx, line) in lines.iter().enumerate() {
        if idx == 1 {
            continue;
        }
        let style = row_style(idx);
        for (i, range) in pad_ranges(&cell_ranges(line), cols, line.len())
            .iter()
            .enumerate()
        {
            let lines = cell_lines(&line[range.start..range.end], range.start, style);
            let width = lines.iter().map(|line| atom_width(line)).max().unwrap_or(0);
            widths[i] = widths[i].max(width.max(MIN_COL_WIDTH));
        }
    }
    lines
        .iter()
        .enumerate()
        .map(|(idx, line)| {
            let style = row_style(idx);
            let ranges = pad_ranges(&cell_ranges(line), cols, line.len());
            let cells = ranges
                .iter()
                .enumerate()
                .map(|(i, range)| {
                    if idx == 1 {
                        vec![fill_atoms("-", widths[i], range.start, range.end, style)]
                    } else {
                        cell_lines(&line[range.start..range.end], range.start, style)
                    }
                })
                .collect();
            compose_row(line, &widths, style, idx == 1, cells, &ranges)
        })
        .collect()
}

fn row_style(idx: usize) -> Style {
    if idx == 0 {
        Style::default().add_modifier(Modifier::BOLD)
    } else if idx == 1 {
        theme::dim()
    } else {
        Style::default()
    }
}

fn compose_row(
    line: &str,
    widths: &[usize],
    style: Style,
    separator: bool,
    cells: Vec<Vec<Vec<Atom>>>,
    ranges: &[CellRange],
) -> MdLine {
    let (hang, indent) = indent_atoms(line);
    let mut groups = vec![Group::Indent(indent)];
    let mut counts = Vec::new();
    let mut line_ws = Vec::new();
    for (i, cell) in cells.into_iter().enumerate() {
        if i > 0 {
            let from = ranges[i - 1].end;
            let to = ranges[i].start;
            groups.push(Group::Gap(fill_atoms(" ", TABLE_GAP, from, to, style)));
        }
        let line_w = cell.iter().map(|line| atom_width(line)).max().unwrap_or(0);
        counts.push(cell.iter().map(Vec::len).collect::<Vec<_>>());
        line_ws.push(line_w);
        groups.push(Group::Cell(cell.into_iter().flatten().collect()));
    }
    seal(&mut groups, line.len());

    let mut indent = Vec::new();
    let mut gaps = Vec::new();
    let mut cell_atoms = Vec::new();
    let mut flat = Vec::new();
    let mut cursor = 0;
    for group in groups {
        match group {
            Group::Indent(atoms) => {
                cursor = atoms.last().map(|atom| atom.end).unwrap_or(cursor);
                flat.extend(atoms.iter().cloned());
                indent = atoms;
            }
            Group::Cell(atoms) => {
                let col = cell_atoms.len();
                let at = atoms.last().map(|atom| atom.end).unwrap_or(cursor);
                let last = col + 1 == widths.len();
                let pad = if last {
                    0
                } else {
                    widths
                        .get(col)
                        .copied()
                        .unwrap_or(MIN_COL_WIDTH)
                        .saturating_sub(line_ws[col])
                };
                flat.extend(atoms.iter().cloned());
                flat.extend(pad_atoms(pad, at, style));
                cursor = at;
                cell_atoms.push(resplit(atoms, &counts[col]));
            }
            Group::Gap(atoms) => {
                cursor = atoms.last().map(|atom| atom.end).unwrap_or(cursor);
                flat.extend(atoms.iter().cloned());
                gaps.push(atoms);
            }
        }
    }
    let (spans, runs) = paint(&flat);
    MdLine {
        spans,
        hang,
        source: Arc::from(line),
        runs: Arc::from(runs),
        table: Some(TableMeta {
            separator,
            style,
            indent,
            widths: widths.to_vec(),
            gaps,
            cells: cell_atoms,
        }),
    }
}

/// Stacks `<br>` lines and, when the row is wider than `avail`, wraps each
/// column inside its own box. `None` when the row is already a single fitted line.
pub(super) fn layout_table(md: &MdLine, avail: usize) -> Option<Vec<MdLine>> {
    let table = md.table.as_ref()?;
    let indent_w = atom_width(&table.indent);
    let gap_w: usize = table.gaps.iter().map(|gap| atom_width(gap)).sum();
    let boxes: usize = table.widths.iter().sum();
    let broken = table.cells.iter().any(|cell| cell.len() > 1);
    if !broken && indent_w + gap_w + boxes <= avail {
        return None;
    }
    let fitted = fit_columns(
        &table.widths,
        avail.saturating_sub(indent_w).saturating_sub(gap_w),
    );
    let wrapped: Vec<Vec<Vec<Atom>>> = table
        .cells
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            let width = fitted
                .get(i)
                .copied()
                .unwrap_or(MIN_COL_WIDTH)
                .max(MIN_COL_WIDTH);
            if table.separator {
                let (start, end) = atom_bounds_lines(cell);
                vec![fill_atoms("-", width, start, end, table.style)]
            } else {
                let mut lines = Vec::new();
                for line in cell {
                    lines.extend(wrap_atoms(line, width));
                }
                if lines.is_empty() {
                    lines.push(Vec::new());
                }
                lines
            }
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let mut lines = Vec::with_capacity(height);
    for row in 0..height {
        let mut atoms = Vec::new();
        if row == 0 {
            atoms.extend(table.indent.iter().cloned());
        } else {
            atoms.extend(blank_atoms(&table.indent));
        }
        for (i, cell_lines) in wrapped.iter().enumerate() {
            if i > 0 {
                let gap = &table.gaps[i - 1];
                if row == 0 {
                    atoms.extend(gap.iter().cloned());
                } else {
                    atoms.extend(blank_atoms(gap));
                }
            }
            let content = cell_lines.get(row).map(Vec::as_slice).unwrap_or(&[]);
            let width = fitted
                .get(i)
                .copied()
                .unwrap_or(MIN_COL_WIDTH)
                .max(MIN_COL_WIDTH);
            let at = content.last().map(|atom| atom.end).unwrap_or(0);
            let last = i + 1 == wrapped.len();
            atoms.extend(content.iter().cloned());
            if !last {
                atoms.extend(pad_atoms(
                    width.saturating_sub(atom_width(content)),
                    at,
                    table.style,
                ));
            }
        }
        lines.push(md_from_atoms(Arc::clone(&md.source), &atoms, 0));
    }
    Some(lines)
}

fn fit_columns(natural: &[usize], budget: usize) -> Vec<usize> {
    let mut widths = natural.to_vec();
    let mut sum: usize = widths.iter().sum();
    if sum <= budget {
        return widths;
    }
    while sum > budget {
        let Some(i) = widths
            .iter()
            .enumerate()
            .filter(|(_, width)| **width > MIN_COL_WIDTH)
            .max_by_key(|(_, width)| *width)
            .map(|(i, _)| i)
        else {
            break;
        };
        widths[i] -= 1;
        sum -= 1;
    }
    widths
}

fn wrap_atoms(atoms: &[Atom], width: usize) -> Vec<Vec<Atom>> {
    if atoms.is_empty() {
        return vec![Vec::new()];
    }
    let width = width.max(MIN_COL_WIDTH);
    let mut lines = Vec::new();
    let mut cur = Vec::new();
    let mut used = 0;
    for atom in atoms {
        let w = atom.width.max(MIN_COL_WIDTH);
        if used > 0 && used + w > width {
            lines.push(std::mem::take(&mut cur));
            used = 0;
        }
        cur.push(atom.clone());
        used += atom.width;
        if used >= width {
            lines.push(std::mem::take(&mut cur));
            used = 0;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

fn seal(groups: &mut [Group], len: usize) {
    let mut prev_end = 0;
    let mut first = true;
    for group in groups.iter_mut() {
        for atom in group.atoms_mut() {
            if first {
                if atom.start > 0 {
                    atom.start = 0;
                }
                first = false;
            } else if atom.start > prev_end {
                atom.start = prev_end;
            }
            if atom.end < atom.start {
                atom.end = atom.start;
            }
            prev_end = atom.end;
        }
    }
    if let Some(last) = groups
        .iter_mut()
        .rev()
        .find_map(|group| group.atoms_mut().last_mut())
        && last.end < len
    {
        last.end = len;
    }
}

fn cell_ranges(line: &str) -> Vec<CellRange> {
    let indent = leading_bytes(line);
    let end = line.trim_end().len();
    if indent >= end {
        return vec![CellRange {
            start: indent,
            end: indent,
        }];
    }
    let mut start = indent;
    let mut stop = end;
    if line.as_bytes().get(start) == Some(&b'|') {
        start += 1;
    }
    if stop > start && line.as_bytes().get(stop - 1) == Some(&b'|') {
        stop -= 1;
    }
    let mut cells = Vec::new();
    let mut cell_at = start;
    for (rel, _) in line[start..stop].match_indices('|') {
        let abs = start + rel;
        cells.push(trim_cell(line, cell_at, abs));
        cell_at = abs + 1;
    }
    cells.push(trim_cell(line, cell_at, stop));
    cells
}

fn pad_ranges(ranges: &[CellRange], cols: usize, len: usize) -> Vec<CellRange> {
    let mut out = ranges.to_vec();
    while out.len() < cols {
        out.push(CellRange {
            start: len,
            end: len,
        });
    }
    out
}

fn trim_cell(line: &str, start: usize, end: usize) -> CellRange {
    let text = &line[start..end];
    let lead = text.len() - text.trim_start().len();
    let trimmed_len = text.trim().len();
    let text_start = start + lead;
    CellRange {
        start: text_start,
        end: text_start + trimmed_len,
    }
}

fn cell_lines(text: &str, offset: usize, style: Style) -> Vec<Vec<Atom>> {
    if text.is_empty() {
        return vec![Vec::new()];
    }
    let breaks = line_breaks(text);
    if breaks.is_empty() {
        return vec![render_cell(text, offset, style)];
    }
    let mut lines = Vec::new();
    let mut cursor = 0;
    let mut pending = offset;
    for br in &breaks {
        let mut atoms = render_cell(&text[cursor..br.start], offset + cursor, style);
        if let Some(atom) = atoms.first_mut()
            && pending < atom.start
        {
            atom.start = pending;
        }
        if let Some(atom) = atoms.last_mut() {
            atom.end = atom.end.max(offset + br.end);
            pending = atom.end;
        } else {
            pending = offset + br.end;
        }
        lines.push(atoms);
        cursor = br.end;
    }
    let mut tail = render_cell(&text[cursor..], offset + cursor, style);
    if let Some(atom) = tail.first_mut()
        && pending < atom.start
    {
        atom.start = pending;
    }
    if !tail.is_empty() {
        lines.push(tail);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

fn line_breaks(text: &str) -> Vec<Range<usize>> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let mut breaks = Vec::new();
    for (event, range) in Parser::new_ext(text, opts).into_offset_iter() {
        let split = match &event {
            Event::HardBreak => true,
            Event::InlineHtml(html) | Event::Html(html) => is_br(html),
            _ => false,
        };
        if split {
            breaks.push(range);
        }
    }
    breaks
}

fn is_br(html: &str) -> bool {
    let Some(inner) = html.trim().strip_prefix('<') else {
        return false;
    };
    let Some(inner) = inner.strip_suffix('>') else {
        return false;
    };
    inner
        .trim()
        .trim_end_matches('/')
        .trim()
        .eq_ignore_ascii_case("br")
}

fn resplit(mut atoms: Vec<Atom>, counts: &[usize]) -> Vec<Vec<Atom>> {
    let mut lines: Vec<Vec<Atom>> = Vec::with_capacity(counts.len());
    for count in counts {
        let n = (*count).min(atoms.len());
        lines.push(atoms.drain(..n).collect());
    }
    if !atoms.is_empty()
        && let Some(last) = lines.last_mut()
    {
        last.append(&mut atoms);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

fn render_cell(text: &str, offset: usize, style: Style) -> Vec<Atom> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut atoms = Vec::new();
    for piece in inline_pieces(text, style) {
        let src_start = offset + piece.range.start;
        let src_end = offset + piece.range.end;
        let origin = &text[piece.origin.start..piece.origin.end];
        let inner = origin.find(&piece.display).unwrap_or(0);
        let disp_start = offset + piece.origin.start + inner;
        atoms.extend(display_atoms(
            &piece.display,
            src_start,
            src_end,
            disp_start,
            piece.style,
        ));
    }
    atoms
}

fn display_atoms(
    display: &str,
    src_start: usize,
    src_end: usize,
    disp_start: usize,
    style: Style,
) -> Vec<Atom> {
    if display.is_empty() {
        return Vec::new();
    }
    let mut atoms = Vec::new();
    let mut byte = disp_start;
    let chars: Vec<char> = display.chars().collect();
    let mut carried = src_start;
    for (i, ch) in chars.iter().enumerate() {
        let len = ch.len_utf8();
        let mut start = if i == 0 { src_start } else { byte };
        let mut end = if i + 1 == chars.len() {
            src_end
        } else {
            byte + len
        };
        if start > end {
            end = start;
        }
        let w = width_of(*ch);
        if w == 0 {
            carried = start;
            byte += len;
            continue;
        }
        if carried < start {
            start = carried;
        }
        carried = end;
        let text = if *ch == '\t' {
            "    ".to_string()
        } else {
            ch.to_string()
        };
        atoms.push(Atom {
            text,
            style,
            width: w,
            start,
            end,
        });
        byte += len;
    }
    atoms
}

fn fill_atoms(
    unit: &str,
    count: usize,
    src_start: usize,
    src_end: usize,
    style: Style,
) -> Vec<Atom> {
    if count == 0 {
        return Vec::new();
    }
    let src_end = src_end.max(src_start);
    let span = src_end - src_start;
    let mut atoms = Vec::with_capacity(count);
    let mut pos = src_start;
    for i in 0..count {
        let next = if i + 1 == count {
            src_end
        } else {
            src_start + span * (i + 1) / count
        };
        atoms.push(Atom {
            text: unit.to_string(),
            style,
            width: unit.width().max(MIN_COL_WIDTH),
            start: pos,
            end: next,
        });
        pos = next;
    }
    atoms
}

fn pad_atoms(count: usize, at: usize, style: Style) -> Vec<Atom> {
    (0..count)
        .map(|_| Atom {
            text: " ".to_string(),
            style,
            width: 1,
            start: at,
            end: at,
        })
        .collect()
}

fn blank_atoms(atoms: &[Atom]) -> Vec<Atom> {
    atoms
        .iter()
        .map(|atom| Atom {
            text: atom.text.clone(),
            style: atom.style,
            width: atom.width,
            start: 0,
            end: 0,
        })
        .collect()
}

fn indent_atoms(line: &str) -> (usize, Vec<Atom>) {
    let bytes = leading_bytes(line);
    let mut atoms = Vec::new();
    let mut i = 0;
    let mut cols = 0;
    while i < bytes {
        let Some(ch) = line[i..].chars().next() else {
            break;
        };
        let len = ch.len_utf8();
        let w = width_of(ch);
        if w > 0 {
            let text = if ch == '\t' {
                "    ".to_string()
            } else {
                ch.to_string()
            };
            atoms.push(Atom {
                text,
                style: Style::default(),
                width: w,
                start: i,
                end: i + len,
            });
            cols += w;
        }
        i += len;
    }
    (cols, atoms)
}

fn paint(atoms: &[Atom]) -> (Vec<Span<'static>>, Vec<Run>) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut runs = Vec::new();
    let mut col = 0;
    for atom in atoms {
        if atom.width == 0 {
            continue;
        }
        runs.push(Run {
            col,
            width: atom.width,
            start: atom.start,
            end: atom.end,
        });
        col += atom.width;
        if atom.text.is_empty() {
            continue;
        }
        if let Some(last) = spans.last_mut()
            && last.style == atom.style
        {
            last.content.to_mut().push_str(&atom.text);
            continue;
        }
        spans.push(if atom.style == Style::default() {
            Span::raw(atom.text.clone())
        } else {
            Span::styled(atom.text.clone(), atom.style)
        });
    }
    if spans.is_empty() {
        spans.push(Span::raw(""));
    }
    (spans, runs)
}

fn md_from_atoms(source: Arc<str>, atoms: &[Atom], hang: usize) -> MdLine {
    let (spans, runs) = paint(atoms);
    MdLine {
        spans,
        hang,
        source,
        runs: Arc::from(runs),
        table: None,
    }
}

fn atom_width(atoms: &[Atom]) -> usize {
    atoms.iter().map(|atom| atom.width).sum()
}

fn atom_bounds_lines(lines: &[Vec<Atom>]) -> (usize, usize) {
    let start = lines
        .iter()
        .flatten()
        .map(|atom| atom.start)
        .min()
        .unwrap_or(0);
    let end = lines
        .iter()
        .flatten()
        .map(|atom| atom.end)
        .max()
        .unwrap_or(start);
    (start, end)
}

fn leading_bytes(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(char::len_utf8)
        .sum()
}

fn leading_width(line: &str) -> usize {
    line.chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(width_of)
        .sum()
}

fn width_of(ch: char) -> usize {
    if ch == '\t' {
        TAB_WIDTH
    } else {
        ch.width().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::*;

    fn show(line: &MdLine) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn source_covered(line: &MdLine) -> String {
        let mut out = String::new();
        let mut at = 0;
        for run in line.runs.iter() {
            assert_eq!(run.start, at, "gap or overlap in {:?}", line.source);
            out.push_str(&line.source[run.start..run.end]);
            at = run.end;
        }
        assert_eq!(at, line.source.len(), "tail dropped in {:?}", line.source);
        out
    }

    #[test]
    fn every_source_byte_is_covered_once() {
        const SAMPLES: &[&str] = &[
            "plain",
            "你好",
            "**bold**",
            "*em* and __nope__",
            "`code`",
            "[lab](http://x)",
            "a **b** c",
            r"\*not",
            "# Title",
            "## Title ##",
            "- item",
            "  - nested",
            "1. ordered",
            "> quote",
            "  > - both",
            "- [ ] box",
            "---",
            "```\nlet x = 1;\n```",
            "| a | b |\n| --- | --- |\n| **c** | d |",
            "|  | b |\n| --- | --- |\n| c | d |",
            "| 你 | 好 |\n| --- | --- |\n| 甲 | 乙 |",
            "**open and tail",
        ];
        for sample in SAMPLES {
            for line in render_markdown(sample) {
                assert_eq!(source_covered(&line), line.source.as_ref(), "{sample:?}");
            }
        }
    }

    #[test]
    fn emphasis_hides_markers_and_keeps_source() {
        let line = &render_markdown("see **bold** and `code`")[0];
        assert_eq!(show(line), "see bold and code");
        assert!(
            line.spans
                .iter()
                .any(|s| s.style.add_modifier == Modifier::BOLD)
        );
        assert!(
            line.spans
                .iter()
                .any(|s| s.style.fg == Some(ratatui::style::Color::Yellow))
        );
        assert_eq!(line.source.as_ref(), "see **bold** and `code`");
    }

    #[test]
    fn unclosed_emphasis_leaves_the_previous_line_alone() {
        let lines = render_markdown("kept\n\n**open and tail");
        assert_eq!(show(&lines[0]), "kept");
        assert_eq!(lines[0].spans[0].style.add_modifier, Modifier::empty());
        let tail = lines.iter().map(show).collect::<String>();
        assert!(tail.contains("tail"), "{tail}");
    }

    #[test]
    fn list_quote_and_heading_markers() {
        assert_eq!(show(&render_markdown("- item")[0]), "• item");
        assert_eq!(show(&render_markdown("> quote")[0]), "│ quote");
        assert_eq!(show(&render_markdown("# Title")[0]), "Title");
        assert_eq!(render_markdown("# Title")[0].source.as_ref(), "# Title");
        assert_eq!(render_markdown("  - nested")[0].hang, 4);
        assert_eq!(render_markdown("  > quote")[0].hang, 4);
    }

    #[test]
    fn selecting_a_table_cell_copies_that_cell() {
        let lines = render_markdown("| left | right |\n| --- | --- |\n| L | R |");
        let header = &lines[0];
        assert_eq!(show(header), "left  right");
        let width = show(header).width();
        let copy = header.copy_window(0, 0, width, true);
        let copied = emit_ranges(&header.source, &copy.ranges_between(0, "left".width()));
        assert!(copied.contains("left"), "{copied}");
        assert!(!copied.contains("right"), "{copied}");
        let all = emit_ranges(&header.source, &copy.ranges_between(0, width));
        assert_eq!(all, "| left | right |");
    }

    #[test]
    fn wide_table_wraps_inside_the_column() {
        let lines = render_markdown("| k | value |\n| --- | --- |\n| a | 0123456789abcdef |");
        let body = lines
            .iter()
            .find(|line| line.source.contains("0123456789"))
            .expect("body row");
        let laid = layout_table(body, 13).expect("column wrap");
        let shown: Vec<String> = laid.iter().map(show).collect();
        assert_eq!(shown, ["a  0123456789", "   abcdef"]);
        let mut ranges = Vec::new();
        for line in &laid {
            let width = show(line).width();
            ranges.extend(line.copy_window(0, 0, width, true).ranges_between(0, width));
        }
        assert_eq!(emit_ranges(&body.source, &ranges), body.source.as_ref());
    }

    #[test]
    fn a_break_in_a_cell_stays_in_its_column() {
        let lines = render_markdown("| k | hi |\n| --- | --- |\n| a | ab<br>cd |");
        let body = lines
            .iter()
            .find(|line| line.source.contains("ab<br>cd"))
            .expect("body row");
        let laid = layout_table(body, 40).expect("cell break");
        let shown: Vec<String> = laid.iter().map(show).collect();
        assert_eq!(shown, ["a  ab", "   cd"]);
        let mut ranges = Vec::new();
        for line in &laid {
            let width = show(line).width();
            ranges.extend(line.copy_window(0, 0, width, true).ranges_between(0, width));
        }
        assert_eq!(emit_ranges(&body.source, &ranges), body.source.as_ref());
    }
}

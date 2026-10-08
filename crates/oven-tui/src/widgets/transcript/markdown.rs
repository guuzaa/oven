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
    /// Source text covered by display columns `[from, to)` of this visual row.
    pub(super) fn slice(&self, from: usize, to: usize) -> String {
        if from >= to {
            return String::new();
        }
        let start = self
            .body_from
            .saturating_add(from.saturating_sub(self.origin))
            .clamp(self.body_from, self.body_to);
        let end = self
            .body_from
            .saturating_add(to.saturating_sub(self.origin))
            .clamp(start, self.body_to);
        let mut out = String::new();
        for run in self.runs.iter() {
            let owned = run.col >= self.body_from && run.col < self.body_to;
            let overlaps = run.col < end && run.col + run.width > start;
            if owned && overlaps {
                out.push_str(&self.source[run.start..run.end]);
            }
        }
        out
    }
}

pub(super) fn copied_text(
    copy: &LineCopy,
    line: &Line<'_>,
    from: usize,
    to: usize,
) -> (bool, String) {
    match copy {
        LineCopy::Visual => (true, extract_line_range(line, from, to)),
        LineCopy::Markdown(md) => (md.break_before, md.slice(from, to)),
    }
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

fn render_table(lines: &[&str]) -> Vec<MdLine> {
    let cells: Vec<Vec<String>> = lines
        .iter()
        .enumerate()
        .map(|(idx, line)| {
            if idx == 1 {
                Vec::new()
            } else {
                split_cells(line)
            }
        })
        .collect();
    let cols = cells.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let mut widths = vec![0; cols];
    for row in &cells {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.width());
        }
    }
    lines
        .iter()
        .enumerate()
        .map(|(idx, line)| {
            let indent = leading_indent(line);
            let body = if idx == 1 {
                widths
                    .iter()
                    .map(|w| "-".repeat((*w).max(1)))
                    .collect::<Vec<_>>()
                    .join(&" ".repeat(TABLE_GAP))
            } else {
                let row = &cells[idx];
                (0..cols)
                    .map(|i| {
                        let cell = row.get(i).map(String::as_str).unwrap_or("");
                        let pad = widths[i].saturating_sub(cell.width());
                        format!("{cell}{}", " ".repeat(pad))
                    })
                    .collect::<Vec<_>>()
                    .join(&" ".repeat(TABLE_GAP))
            };
            let style = if idx == 0 {
                Style::default().add_modifier(Modifier::BOLD)
            } else if idx == 1 {
                theme::dim()
            } else {
                Style::default()
            };
            table_line(line, &indent, &body, style)
        })
        .collect()
}

fn table_line(source: &str, indent: &str, body: &str, style: Style) -> MdLine {
    let display = format!("{indent}{body}");
    let width = display.width();
    let mut spans = Vec::new();
    if !indent.is_empty() {
        spans.push(Span::raw(indent.to_string()));
    }
    if !body.is_empty() {
        spans.push(if style == Style::default() {
            Span::raw(body.to_string())
        } else {
            Span::styled(body.to_string(), style)
        });
    }
    if spans.is_empty() {
        spans.push(Span::raw(""));
    }
    let runs = if width == 0 {
        Vec::new()
    } else {
        vec![Run {
            col: 0,
            width,
            start: 0,
            end: source.len(),
        }]
    };
    MdLine {
        spans,
        hang: indent.width(),
        source: Arc::from(source),
        runs: Arc::from(runs),
    }
}

fn split_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim().trim_start_matches('|').trim_end_matches('|');
    trimmed
        .split('|')
        .map(|cell| inline_display(cell.trim()))
        .collect()
}

fn inline_display(content: &str) -> String {
    inline_pieces(content, Style::default())
        .into_iter()
        .map(|piece| piece.display)
        .collect()
}

fn leading_indent(line: &str) -> String {
    let mut out = String::new();
    for ch in line.chars() {
        if ch == ' ' {
            out.push(' ');
        } else if ch == '\t' {
            out.push_str("    ");
        } else {
            break;
        }
    }
    out
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
}

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
const ASTERISK: char = '*';
const UNDERSCORE: char = '_';
const TILDE: char = '~';
const MARKER_CHARS: [char; 3] = [ASTERISK, UNDERSCORE, TILDE];
const BOLD_LEN: usize = 2;
const FENCE_MIN: usize = 3;
const HEADING_MAX: usize = 6;
const TAB_WIDTH: usize = 4;
const TAB_SPACES: &str = "    ";
const TABLE_GAP: usize = 2;
const MIN_COL_WIDTH: usize = 1;
const TASK_BOX_LEN: usize = 3;

#[derive(Clone)]
struct Run {
    col: usize,
    width: usize,
    start: usize,
    end: usize,
}

/// One drawn glyph with the source bytes it came from. Markdown lines and
/// table cells both build these; `paint` turns them into spans and copy runs.
/// Invisible chars such as emoji joiners ride in a neighbour's text and only
/// reach zero width when a run has no visible glyph at all.
#[derive(Clone)]
struct Atom {
    text: Glyph,
    style: Style,
    width: usize,
    start: usize,
    end: usize,
}

/// The characters an atom draws. An atom is usually one character of the
/// source, so the glyph stays a `char` and only becomes owned text when it
/// does not fit that shape.
#[derive(Clone)]
enum Glyph {
    /// One character.
    Char(char),
    /// A fixed run of characters: a tab draws as spaces.
    Fixed(&'static str),
    /// Characters that joined a neighbour, such as a zero-width joiner.
    Text(String),
}

impl Glyph {
    /// Appends the glyph to `text`.
    fn push_into(&self, text: &mut String) {
        match self {
            Glyph::Char(ch) => text.push(*ch),
            Glyph::Fixed(fixed) => text.push_str(fixed),
            Glyph::Text(owned) => text.push_str(owned),
        }
    }
}

/// A glyph for text that is not one character of the source.
fn fixed_glyph(text: &str) -> Glyph {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => Glyph::Char(ch),
        _ => Glyph::Text(text.to_string()),
    }
}

/// Joins two glyphs into one owned run.
fn append_glyph(target: &mut Glyph, extra: &Glyph) {
    let mut joined = String::new();
    target.push_into(&mut joined);
    extra.push_into(&mut joined);
    *target = Glyph::Text(joined);
}

#[derive(Clone)]
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
        let column = |col: usize| {
            self.body_from
                .saturating_add(col.saturating_sub(self.origin))
                .clamp(self.body_from, self.body_to)
        };
        let start = column(from);
        let end = column(to).max(start);
        self.runs
            .iter()
            .filter(|run| {
                run.col >= self.body_from
                    && run.col < self.body_to
                    && run.col < end
                    && run.col + run.width > start
                    && run.start < run.end
            })
            .map(|run| run.start..run.end)
            .collect()
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
                if !newline
                    && let Some(pending) = self.pending.as_mut()
                    && Arc::ptr_eq(&pending.source, &md.source)
                {
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
        (!self.out.is_empty()).then_some(self.out)
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
    for range in spans {
        let start = range.start.min(len).max(covered);
        let end = range.end.min(len);
        if start < end {
            out.push_str(&source[start..end]);
            covered = end;
        }
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
        if let Some(open) = fence.as_ref() {
            let closed = closes_fence(line, open);
            let style = if closed { theme::dim() } else { theme::code() };
            out.push(verbatim(line, style, leading_width(line)));
            if closed {
                fence = None;
            }
        } else if let Some(open) = opening_fence(line) {
            fence = Some(open);
            out.push(verbatim(line, theme::dim(), leading_width(line)));
        } else if let Some(n) = table_len(&lines[i..]) {
            out.extend(render_table(&lines[i..i + n]));
            i += n;
            continue;
        } else {
            out.push(render_flow(line));
        }
        i += 1;
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
    let marker = *bytes.first()?;
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
    let hang = leading_width(&line[..indent_at]);
    let (shown, hidden) = strip_atx_closer(&line[content_at..]);
    if shown.is_empty() {
        return md_line(line, source_atoms(line, theme::dim()), hang);
    }
    let mut atoms = source_atoms(&line[..indent_at], Style::default());
    atoms.extend(piece_atoms(
        shown,
        inline_pieces(shown, theme::heading(level)),
        content_at,
    ));
    // Hashes and a closing `#` sequence are not drawn; they stay on the edge atoms.
    if let Some(atom) = atoms.iter_mut().find(|atom| atom.start >= indent_at) {
        atom.start = indent_at;
    }
    if !hidden.is_empty()
        && let Some(last) = atoms.last_mut()
    {
        last.end = line.len();
    }
    md_line(line, atoms, hang)
}

fn render_marked(line: &str) -> MdLine {
    let bytes = line.as_bytes();
    let mut atoms = Vec::new();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    atoms.extend(source_atoms(&line[..i], Style::default()));
    let mut quoted = false;
    while i < bytes.len() && bytes[i] == b'>' {
        quoted = true;
        atoms.push(glyph_atom(QUOTE_BAR, i, i + 1, theme::dim()));
        i += 1;
        if bytes.get(i) == Some(&b' ') {
            atoms.push(glyph_atom(' ', i, i + 1, Style::default()));
            i += 1;
        }
    }
    if let Some(marker_end) = list_marker_end(&line[i..]) {
        atoms.extend(marker_atoms(&line[i..i + marker_end], i));
        i += marker_end;
    }
    let hang = atom_width(&atoms);
    let content = &line[i..];
    if !content.is_empty() {
        let base = if quoted {
            Style::default().add_modifier(Modifier::ITALIC)
        } else {
            Style::default()
        };
        atoms.extend(piece_atoms(content, inline_pieces(content, base), i));
    }
    md_line(line, atoms, hang)
}

fn marker_atoms(marker: &str, src_at: usize) -> Vec<Atom> {
    marker
        .char_indices()
        .map(|(off, ch)| {
            let at = src_at + off;
            let accent = off == 0 || ch.is_ascii_digit() || ch == '.' || ch == ')';
            let shown = if off == 0 && matches!(ch, '-' | '*' | '+') {
                BULLET
            } else {
                ch
            };
            let style = if accent {
                theme::accent()
            } else {
                Style::default()
            };
            glyph_atom(shown, at, at + ch.len_utf8(), style)
        })
        .collect()
}

fn list_marker_end(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let marker = match bytes.first()? {
        b'-' | b'*' | b'+' => 1,
        _ => {
            let digits = bytes
                .iter()
                .take(9)
                .take_while(|b| b.is_ascii_digit())
                .count();
            if digits == 0 || !matches!(bytes.get(digits), Some(b'.' | b')')) {
                return None;
            }
            digits + 1
        }
    };
    if !matches!(bytes.get(marker), None | Some(b' ' | b'\t')) {
        return None;
    }
    let mut end = marker;
    while matches!(bytes.get(end), Some(b' ' | b'\t')) {
        end += 1;
    }
    if task_box(&bytes[end..]) {
        end = skip_blanks(bytes, end + TASK_BOX_LEN);
    }
    Some(end)
}

fn task_box(rest: &[u8]) -> bool {
    matches!(rest, [b'[', b' ' | b'x' | b'X', b']', ..])
}

fn skip_blanks(bytes: &[u8], mut at: usize) -> usize {
    while matches!(bytes.get(at), Some(b' ' | b'\t')) {
        at += 1;
    }
    at
}

struct Piece {
    display: String,
    origin: Range<usize>,
    range: Range<usize>,
    style: Style,
    /// Code and inline html are shown as parsed; only markdown text can still
    /// hold delimiter runs pulldown-cmark refused to pair.
    verbatim: bool,
}

fn inline_parser(text: &str) -> Parser<'_> {
    Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH)
}

fn inline_pieces(content: &str, base: Style) -> Vec<Piece> {
    let mut style = base;
    let mut stack = Vec::new();
    let mut marks = Vec::new();
    let mut pieces = Vec::new();
    for (event, range) in inline_parser(content).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if let Some(next) = inline_style(base, &tag) {
                    marks.push(range.clone());
                    stack.push(style);
                    style = next;
                }
            }
            Event::End(tag) => {
                if inline_end(&tag) {
                    style = stack.pop().unwrap_or(base);
                }
            }
            Event::Text(text) => pieces.push(text_piece(text.into_string(), range, style)),
            Event::Code(text) => {
                pieces.push(verbatim_piece(text.into_string(), range, theme::code()))
            }
            Event::InlineHtml(text) | Event::Html(text) => {
                pieces.push(verbatim_piece(text.into_string(), range, theme::dim()));
            }
            _ => {}
        }
    }
    if pieces.is_empty() {
        return if content.is_empty() {
            pieces
        } else {
            vec![text_piece(content.to_string(), 0..content.len(), base)]
        };
    }
    pair_stray_markers(&mut pieces, content);
    cover_gaps(&mut pieces, &marks, content.len());
    pieces
}

fn text_piece(display: String, range: Range<usize>, style: Style) -> Piece {
    Piece {
        display,
        origin: range.clone(),
        range,
        style,
        verbatim: false,
    }
}

fn verbatim_piece(display: String, range: Range<usize>, style: Style) -> Piece {
    Piece {
        display,
        origin: range.clone(),
        range,
        style,
        verbatim: true,
    }
}

fn inline_style(base: Style, tag: &Tag<'_>) -> Option<Style> {
    Some(match tag {
        Tag::Strong => base.add_modifier(Modifier::BOLD),
        Tag::Emphasis => base.add_modifier(Modifier::ITALIC),
        Tag::Strikethrough => base.add_modifier(Modifier::CROSSED_OUT),
        Tag::Link { .. } | Tag::Image { .. } => theme::link(),
        _ => return None,
    })
}

fn inline_end(tag: &TagEnd) -> bool {
    matches!(
        tag,
        TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link | TagEnd::Image
    )
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

struct StrayMarker {
    pieces: Range<usize>,
    source: Range<usize>,
    ch: char,
    len: usize,
    modifier: Modifier,
    opens: bool,
    closes: bool,
}

/// CommonMark flanking rules leave `**“x”**` unparsed, so pair the delimiter
/// runs pulldown-cmark gave up on and hide them like the ones it consumed.
fn pair_stray_markers(pieces: &mut Vec<Piece>, content: &str) {
    let markers = stray_markers(pieces, content);
    let mut pending: Vec<usize> = Vec::new();
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (idx, marker) in markers.iter().enumerate() {
        if marker.closes
            && let Some(pos) = pending
                .iter()
                .rposition(|&open| markers[open].ch == marker.ch && markers[open].len == marker.len)
        {
            pairs.push((pending[pos], idx));
            pending.truncate(pos);
        }
        if marker.opens {
            pending.push(idx);
        }
    }
    for (head, tail) in pairs.into_iter().rev() {
        hide_pair(pieces, &markers[head], &markers[tail]);
    }
}

fn hide_pair(pieces: &mut Vec<Piece>, opener: &StrayMarker, closer: &StrayMarker) {
    let first = opener.pieces.end;
    let Some(last) = closer.pieces.start.checked_sub(1) else {
        return;
    };
    if first > last || last >= pieces.len() {
        return;
    }
    for piece in &mut pieces[first..=last] {
        piece.style = piece.style.add_modifier(opener.modifier);
    }
    pieces[first].range.start = opener.source.start;
    pieces[last].range.end = closer.source.end;
    pieces.drain(closer.pieces.clone());
    pieces.drain(opener.pieces.clone());
}

fn stray_markers(pieces: &[Piece], content: &str) -> Vec<StrayMarker> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < pieces.len() {
        let Some(ch) = delimiter_char(&pieces[i]) else {
            i += 1;
            continue;
        };
        let first = i;
        let mut last = i;
        while last + 1 < pieces.len()
            && delimiter_char(&pieces[last + 1]) == Some(ch)
            && pieces[last + 1].range.start == pieces[last].range.end
        {
            last += 1;
        }
        let source = pieces[first].range.start..pieces[last].range.end;
        let len: usize = pieces[first..=last]
            .iter()
            .map(|piece| piece.display.chars().count())
            .sum();
        i = last + 1;
        let Some(modifier) = marker_modifier(ch, len) else {
            continue;
        };
        let before = content[..source.start].chars().next_back();
        let after = content[source.end..].chars().next();
        // `_` never works intraword, the other two do.
        let locked = ch == UNDERSCORE;
        out.push(StrayMarker {
            pieces: first..last + 1,
            source,
            ch,
            len,
            modifier,
            opens: after.is_some_and(|c| !c.is_whitespace())
                && (!locked || !before.is_some_and(|c| c.is_alphanumeric())),
            closes: before.is_some_and(|c| !c.is_whitespace())
                && (!locked || !after.is_some_and(|c| c.is_alphanumeric())),
        });
    }
    out
}

fn delimiter_char(piece: &Piece) -> Option<char> {
    if piece.verbatim {
        return None;
    }
    let mut chars = piece.display.chars();
    let first = chars.next()?;
    (MARKER_CHARS.contains(&first) && chars.all(|c| c == first)).then_some(first)
}

fn marker_modifier(ch: char, len: usize) -> Option<Modifier> {
    match ch {
        ASTERISK | UNDERSCORE if len <= BOLD_LEN => Some(if len == BOLD_LEN {
            Modifier::BOLD
        } else {
            Modifier::ITALIC
        }),
        TILDE if len == BOLD_LEN => Some(Modifier::CROSSED_OUT),
        _ => None,
    }
}

fn piece_atoms(content: &str, pieces: Vec<Piece>, offset: usize) -> Vec<Atom> {
    let mut atoms: Vec<Atom> = Vec::new();
    for piece in pieces {
        let src_end = offset + piece.range.end;
        if piece.display.is_empty() {
            if let Some(last) = atoms.last_mut() {
                last.end = last.end.max(src_end);
            }
            continue;
        }
        let origin = &content[piece.origin.clone()];
        let inner = origin.find(&piece.display).unwrap_or(0);
        let disp_start = offset + piece.origin.start + inner;
        atoms.extend(display_atoms(
            &piece.display,
            offset + piece.range.start,
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
    let mut atoms: Vec<Atom> = Vec::new();
    let mut leading: Option<Atom> = None;
    let mut byte = disp_start;
    for (i, ch) in display.char_indices() {
        let start = if i == 0 { src_start } else { byte };
        let end = if i + ch.len_utf8() == display.len() {
            src_end
        } else {
            (byte + ch.len_utf8()).max(start)
        };
        byte += ch.len_utf8();
        let atom = glyph_atom(ch, start, end, style);
        if atom.width == 0 {
            match atoms.last_mut() {
                Some(last) => {
                    append_glyph(&mut last.text, &atom.text);
                    last.end = last.end.max(atom.end);
                }
                None => match leading.as_mut() {
                    Some(lead) => {
                        append_glyph(&mut lead.text, &atom.text);
                        lead.end = atom.end;
                    }
                    None => leading = Some(atom),
                },
            }
            continue;
        }
        match leading.take() {
            Some(mut lead) => {
                append_glyph(&mut lead.text, &atom.text);
                lead.width = atom.width;
                lead.end = atom.end;
                atoms.push(lead);
            }
            None => atoms.push(atom),
        }
    }
    if let Some(lead) = leading {
        atoms.push(lead);
    }
    atoms
}

fn source_atoms(line: &str, style: Style) -> Vec<Atom> {
    line.char_indices()
        .filter_map(|(at, ch)| {
            let atom = glyph_atom(ch, at, at + ch.len_utf8(), style);
            (atom.width > 0).then_some(atom)
        })
        .collect()
}

fn glyph_atom(ch: char, start: usize, end: usize, style: Style) -> Atom {
    Atom {
        text: if ch == '\t' {
            Glyph::Fixed(TAB_SPACES)
        } else {
            Glyph::Char(ch)
        },
        style,
        width: width_of(ch),
        start,
        end,
    }
}

fn md_line(source: impl Into<Arc<str>>, atoms: Vec<Atom>, hang: usize) -> MdLine {
    let (spans, runs) = paint(atoms);
    MdLine {
        spans,
        hang,
        source: source.into(),
        runs: Arc::from(runs),
        table: None,
    }
}

fn verbatim(line: &str, style: Style, hang: usize) -> MdLine {
    md_line(line, source_atoms(line, style), hang)
}

fn blank_line(line: &str) -> MdLine {
    md_line(line, Vec::new(), 0)
}

fn atx_heading(line: &str) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let indent = bytes
        .iter()
        .take_while(|b| matches!(b, b' ' | b'\t'))
        .count();
    let level = bytes[indent..].iter().take_while(|b| **b == b'#').count();
    if level == 0 || level > HEADING_MAX {
        return None;
    }
    let content = indent + level;
    if !matches!(bytes.get(content), None | Some(b' ' | b'\t')) {
        return None;
    }
    Some((level, (content + 1).min(line.len())))
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
    if !trimmed.contains('-') {
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

fn render_table(lines: &[&str]) -> Vec<MdLine> {
    let raw: Vec<Vec<CellRange>> = lines.iter().map(|line| cell_ranges(line)).collect();
    let cols = raw.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let mut widths = vec![MIN_COL_WIDTH; cols];
    let mut rows = Vec::with_capacity(lines.len());
    for (idx, (line, mut ranges)) in lines.iter().zip(raw).enumerate() {
        let style = row_style(idx);
        ranges.resize(
            cols,
            CellRange {
                start: line.len(),
                end: line.len(),
            },
        );
        let cells: Vec<Option<Vec<Vec<Atom>>>> = ranges
            .iter()
            .map(|range| {
                (idx != 1).then(|| cell_lines(&line[range.start..range.end], range.start, style))
            })
            .collect();
        if idx != 1 {
            for (width, cell) in widths.iter_mut().zip(&cells) {
                let natural = cell.iter().flatten().map(|line| atom_width(line)).max();
                *width = (*width).max(natural.unwrap_or(0));
            }
        }
        rows.push((*line, ranges, cells));
    }
    rows.into_iter()
        .enumerate()
        .map(|(idx, (line, ranges, cells))| {
            let style = row_style(idx);
            let separator = idx == 1;
            let cells = cells
                .into_iter()
                .enumerate()
                .map(|(i, cell)| {
                    cell.unwrap_or_else(|| {
                        vec![fill_atoms(
                            "-",
                            widths[i],
                            ranges[i].start,
                            ranges[i].end,
                            style,
                        )]
                    })
                })
                .collect();
            compose_row(line, &widths, style, separator, cells, &ranges)
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
    let indent_len = indent.len();
    let mut flat = indent;
    let mut gap_spans = Vec::with_capacity(cells.len().saturating_sub(1));
    let mut cell_spans = Vec::with_capacity(cells.len());
    let mut line_ws = Vec::with_capacity(cells.len());
    for (i, cell) in cells.into_iter().enumerate() {
        if i > 0 {
            gap_spans.push((flat.len(), TABLE_GAP));
            flat.extend(fill_atoms(
                " ",
                TABLE_GAP,
                ranges[i - 1].end,
                ranges[i].start,
                style,
            ));
        }
        line_ws.push(cell.iter().map(|line| atom_width(line)).max().unwrap_or(0));
        let counts = cell.iter().map(Vec::len).collect::<Vec<_>>();
        let start = flat.len();
        flat.extend(cell.into_iter().flatten());
        cell_spans.push((start, flat.len() - start, counts));
        if i + 1 < widths.len() {
            let at = flat.last().map(|atom| atom.end).unwrap_or(0);
            let pad = widths[i].saturating_sub(line_ws[i]);
            flat.extend(fill_atoms(" ", pad, at, at, style));
        }
    }
    seal(&mut flat, line.len());
    let indent = flat[..indent_len].to_vec();
    let gaps = gap_spans
        .iter()
        .map(|&(at, len)| flat[at..at + len].to_vec())
        .collect();
    let cells = cell_spans
        .iter()
        .map(|(at, len, counts)| resplit(flat[*at..*at + len].to_vec(), counts))
        .collect();
    let (spans, runs) = paint(flat);
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
            cells,
        }),
    }
}

/// Stacks `<br>` lines and, when the row is wider than `avail`, wraps each
/// column inside its own box. `None` when the row is already a single fitted line.
pub(super) fn layout_table(md: &MdLine, avail: usize) -> Option<Vec<MdLine>> {
    let table = md.table.as_ref()?;
    let indent_w = atom_width(&table.indent);
    let gap_w: usize = table.gaps.iter().map(|gap| atom_width(gap)).sum();
    let natural: usize = table.widths.iter().sum();
    let stacked = table.cells.iter().any(|cell| cell.len() > 1);
    if !stacked && indent_w + gap_w + natural <= avail {
        return None;
    }
    let widths = fit_columns(
        &table.widths,
        avail.saturating_sub(indent_w).saturating_sub(gap_w),
    );
    let wrapped: Vec<Vec<Vec<Atom>>> = table
        .cells
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            let width = widths[i];
            if table.separator {
                let (start, end) = atom_bounds_lines(cell);
                vec![fill_atoms("-", width, start, end, table.style)]
            } else {
                let mut lines: Vec<Vec<Atom>> = cell
                    .iter()
                    .flat_map(|line| wrap_atoms(line, width))
                    .collect();
                if lines.is_empty() {
                    lines.push(Vec::new());
                }
                lines
            }
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    let mut lines = Vec::with_capacity(height);
    for row in 0..height {
        let mut atoms = if row == 0 {
            table.indent.clone()
        } else {
            blank_atoms(&table.indent)
        };
        for (i, cell) in wrapped.iter().enumerate() {
            if i > 0 {
                let gap = &table.gaps[i - 1];
                atoms.extend(if row == 0 {
                    gap.clone()
                } else {
                    blank_atoms(gap)
                });
            }
            let content = cell.get(row).map(Vec::as_slice).unwrap_or_default();
            let at = content.last().map(|atom| atom.end).unwrap_or(0);
            atoms.extend_from_slice(content);
            if i + 1 < wrapped.len() {
                let pad = widths[i].saturating_sub(atom_width(content));
                atoms.extend(fill_atoms(" ", pad, at, at, table.style));
            }
        }
        lines.push(md_line(Arc::clone(&md.source), atoms, 0));
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
    let width = width.max(MIN_COL_WIDTH);
    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut used = 0;
    for atom in atoms {
        if used > 0 && used + atom.width > width {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        line.push(atom.clone());
        used += atom.width;
        if used >= width {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// Makes the atoms' source ranges contiguous, so a run of atoms covers the
/// line without gaps. Padding and filled separators have no source of their own
/// and inherit the range they are inserted at.
fn seal(atoms: &mut [Atom], len: usize) {
    let mut prev_end = 0;
    for (i, atom) in atoms.iter_mut().enumerate() {
        if i == 0 {
            atom.start = 0;
        } else if atom.start > prev_end {
            atom.start = prev_end;
        }
        if atom.end < atom.start {
            atom.end = atom.start;
        }
        prev_end = atom.end;
    }
    if let Some(last) = atoms.last_mut()
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
    let mut lines = Vec::new();
    let mut cursor = 0;
    for br in line_breaks(text) {
        lines.push(cell_segment(
            &text[cursor..br.start],
            offset + cursor,
            style,
            offset + br.end,
        ));
        cursor = br.end;
    }
    let tail = cell_segment(&text[cursor..], offset + cursor, style, offset + text.len());
    if !tail.is_empty() {
        lines.push(tail);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

/// Renders one side of a `<br>` and extends its source range over the break.
fn cell_segment(text: &str, offset: usize, style: Style, end: usize) -> Vec<Atom> {
    let mut atoms = render_cell(text, offset, style);
    if let Some(first) = atoms.first_mut()
        && offset < first.start
    {
        first.start = offset;
    }
    if let Some(last) = atoms.last_mut() {
        last.end = last.end.max(end);
    }
    atoms
}

fn line_breaks(text: &str) -> Vec<Range<usize>> {
    inline_parser(text)
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            Event::HardBreak => Some(range),
            Event::InlineHtml(html) | Event::Html(html) if is_br(&html) => Some(range),
            _ => None,
        })
        .collect()
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

fn resplit(atoms: Vec<Atom>, counts: &[usize]) -> Vec<Vec<Atom>> {
    let mut atoms = atoms.into_iter();
    let mut lines: Vec<Vec<Atom>> = counts
        .iter()
        .map(|count| atoms.by_ref().take(*count).collect())
        .collect();
    if let Some(last) = lines.last_mut() {
        last.extend(atoms);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

fn render_cell(text: &str, offset: usize, style: Style) -> Vec<Atom> {
    piece_atoms(text, inline_pieces(text, style), offset)
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
    let width = unit.width().max(MIN_COL_WIDTH);
    let mut atoms = Vec::with_capacity(count);
    let mut pos = src_start;
    for i in 0..count {
        let next = if i + 1 == count {
            src_end
        } else {
            src_start + span * (i + 1) / count
        };
        atoms.push(Atom {
            text: fixed_glyph(unit),
            style,
            width,
            start: pos,
            end: next,
        });
        pos = next;
    }
    atoms
}

fn blank_atoms(atoms: &[Atom]) -> Vec<Atom> {
    atoms
        .iter()
        .map(|atom| Atom {
            start: 0,
            end: 0,
            ..atom.clone()
        })
        .collect()
}

fn indent_atoms(line: &str) -> (usize, Vec<Atom>) {
    let atoms = source_atoms(&line[..leading_bytes(line)], Style::default());
    (atom_width(&atoms), atoms)
}

fn paint(atoms: Vec<Atom>) -> (Vec<Span<'static>>, Vec<Run>) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut runs = Vec::new();
    let mut col = 0;
    for atom in atoms {
        if atom.width > 0 {
            runs.push(Run {
                col,
                width: atom.width,
                start: atom.start,
                end: atom.end,
            });
            col += atom.width;
        }
        let style = atom.style;
        let same_style = spans.last().is_some_and(|last| last.style == style);
        if same_style {
            if let Some(last) = spans.last_mut() {
                let mut content = last.content.to_mut();
                atom.text.push_into(&mut content);
            }
        } else {
            let mut content = String::new();
            atom.text.push_into(&mut content);
            spans.push(Span::styled(content, style));
        }
    }
    if spans.is_empty() {
        spans.push(Span::raw(""));
    }
    (spans, runs)
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
            "两者是**同一个 API 的“实现”与“懒加载入口”**关系",
            "x**“实现”**y",
            "2 * 3 * 4",
            "call foo_bar_baz now",
            "use `**` here",
            "👨‍👩‍👧‍👦 family ❤️ 🇨🇳",
            "e\u{301} combining",
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
    fn cjk_punctuation_does_not_block_emphasis() {
        const TEXT: &str = "两者是**同一个 API 的“实现”与“懒加载入口”**关系";
        let line = &render_markdown(TEXT)[0];
        assert_eq!(show(line), "两者是同一个 API 的“实现”与“懒加载入口”关系");
        assert!(
            line.spans
                .iter()
                .any(|s| s.style.add_modifier == Modifier::BOLD)
        );
        assert_eq!(line.source.as_ref(), TEXT);
        let width = show(line).width();
        let copy = line.copy_window(0, 0, width, true);
        assert_eq!(
            emit_ranges(&line.source, &copy.ranges_between(0, width)),
            TEXT
        );
    }

    #[test]
    fn stray_markers_need_adjacent_text() {
        assert_eq!(show(&render_markdown("2 * 3 * 4")[0]), "2 * 3 * 4");
        assert_eq!(
            show(&render_markdown("call foo_bar_baz now")[0]),
            "call foo_bar_baz now"
        );
        assert_eq!(show(&render_markdown("use `**` here")[0]), "use ** here");
        assert_eq!(show(&render_markdown("a`**`b`**`c")[0]), "a**b**c");
        assert_eq!(show(&render_markdown("a **“b”** c")[0]), "a “b” c");
        assert_eq!(show(&render_markdown("~~“gone”~~ x")[0]), "“gone” x");
    }

    #[test]
    fn invisible_glyphs_stay_in_the_text() {
        const FAMILY: &str = "👨‍👩‍👧‍👦";
        let line = &render_markdown(FAMILY)[0];
        assert_eq!(show(line), FAMILY);
        assert_eq!(source_covered(line), FAMILY);

        let lines = render_markdown("| ❤️ | b |\n| --- | --- |\n| 👨‍👩‍👧‍👦 | x |");
        let shown = lines.iter().map(show).collect::<Vec<_>>().join("\n");
        assert!(shown.contains(FAMILY), "{shown}");
        assert!(shown.contains("❤️"), "{shown}");
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

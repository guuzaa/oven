use std::f32::consts::TAU;
use std::mem;
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::theme;
use super::collapsible::{Collapsible, Section};
use super::kinds::{
    COLLAPSED_MARKER, EXPANDED_MARKER, Header, LINE_INDENT, LineKind, MESSAGE_INDENT,
};

pub(super) const MAX_SHELL_DISPLAY_LINES: usize = 100;
/// Screen rows — counted after wrapping — a body still receiving content may
/// occupy, so a line that wraps into many rows is capped by the rows it takes,
/// not by the newline it came from.
pub(super) const MAX_LIVE_BODY_ROWS: usize = 8;
pub(super) const THINKING_LABEL: &str = "Thinking...";
pub(super) const THOUGHT_LABEL: &str = "Thought";
pub(super) const RESULT_LABEL: &str = "Result";
const EARLIER_LINES: &str = "earlier lines";
const MS_PER_SECOND: u64 = 1000;
const MS_PER_TENTH: u64 = 100;
const TENTHS_PER_SECOND: u64 = 10;
const MS_PER_MINUTE: u64 = 60_000;
const PERIOD_MS: u128 = 1400;
const PERIOD: f32 = 1400.0;
const SHADE_MIN: f32 = 88.0;
const SHADE_MAX: f32 = 220.0;
const WORKED_FOR: &str = "Worked for";
const THOUGHT_FOR: &str = "Thought for";
/// Rows the … N earlier lines marker occupies inside a live body budget.
const MARKER_ROWS: usize = 1;
/// Border plus one padding column on each side of a framed prompt.
const PROMPT_FRAME_COLS: usize = 4;
const PROMPT_CORNER_COLS: usize = 2;

fn format_duration(ms: u64) -> String {
    let mins = ms / MS_PER_MINUTE;
    let rest = ms % MS_PER_MINUTE;
    let secs = if mins == 0 {
        let tenths = rest / MS_PER_TENTH;
        let whole = tenths / TENTHS_PER_SECOND;
        let frac = tenths % TENTHS_PER_SECOND;
        if frac == 0 {
            whole.to_string()
        } else {
            format!("{whole}.{frac}")
        }
    } else {
        (rest / MS_PER_SECOND).to_string()
    };
    if mins == 0 {
        format!("{secs}s")
    } else {
        format!("{mins}m {secs}s")
    }
}

/// Trailing line of a finished turn: how long the agent took, or nothing at all
/// when the duration is unknown.
pub(super) fn format_elapsed(ms: u64) -> String {
    format!("{WORKED_FOR} {}", format_duration(ms))
}

/// Header for a thinking row: the duration the agent reported, or a bare label
/// when the row carries no timed span.
pub(super) fn format_thought(ms: Option<u64>) -> String {
    match ms {
        Some(ms) => format!("{THOUGHT_FOR} {}", format_duration(ms)),
        None => THOUGHT_LABEL.to_string(),
    }
}

pub(super) fn thinking_display_label(text: &str) -> &str {
    if text == THINKING_LABEL || text.starts_with(THOUGHT_FOR) {
        text
    } else {
        THOUGHT_LABEL
    }
}

pub(crate) fn paint_visible(f: &mut Frame<'_>, area: Rect, lines: Vec<Line<'static>>) {
    f.render_widget(Paragraph::new(lines), area);
    // Wide CJK glyphs leave a stale trailing cell on Windows; force a full paint.
    let buf = f.buffer_mut();
    let area = area.intersection(*buf.area());
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)].set_diff_option(CellDiffOption::AlwaysUpdate);
        }
    }
}

pub(super) fn collect_lines(
    history: &[Line<'static>],
    stream: &[Line<'static>],
    start: usize,
    end: usize,
) -> Vec<Line<'static>> {
    let n = history.len();
    let cut = start.min(n).min(end);
    let h = &history[cut..end.min(n)];
    let s = &stream[start.saturating_sub(n)..end.saturating_sub(n)];
    h.iter().chain(s).cloned().collect()
}

pub(super) fn trim_message(text: &str) -> String {
    text.trim_matches(|c: char| c == '\n' || c == '\r')
        .to_string()
}

/// Fraction of the shimmer period the current time sits at.
#[allow(
    clippy::cast_precision_loss,
    reason = "the offset is reduced modulo PERIOD_MS, which is exact in f32"
)]
pub(super) fn thinking_phase() -> f32 {
    let phase_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() % PERIOD_MS);
    phase_ms as f32 / PERIOD
}

pub(super) fn apply_thinking_shimmer(line: &Line<'static>, phase: f32) -> Line<'static> {
    match line.spans.as_slice() {
        [head, rest @ ..] => {
            let body: String = rest.iter().map(|s| s.content.as_ref()).collect();
            if body.is_empty() {
                return line.clone();
            }
            let mut spans = vec![head.clone()];
            spans.extend(shimmer_body(&body, phase));
            Line::from(spans)
        }
        _ => line.clone(),
    }
}

fn shimmer_body(text: &str, phase: f32) -> Vec<Span<'static>> {
    let n = to_f32(text.chars().count()).max(1.0);
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            let wave = ((to_f32(i) / n - phase) * TAU).cos() * 0.5 + 0.5;
            Span::styled(ch.to_string(), Style::default().fg(thinking_shade(wave)))
        })
        .collect()
}

/// Character offsets in `f32`; shimmer lines are far too short for the
/// saturation to be observable.
#[inline]
fn to_f32(n: usize) -> f32 {
    f32::from(u16::try_from(n).unwrap_or(u16::MAX))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the clamped shade always lands inside the u8 range"
)]
fn thinking_shade(t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let shade = (SHADE_MIN + (SHADE_MAX - SHADE_MIN) * t) as u8;
    Color::Rgb(shade, shade, shade)
}

fn earlier_lines(skipped: usize) -> String {
    format!("… {skipped} {EARLIER_LINES}")
}

fn earlier_lines_marker(skipped: usize) -> Line<'static> {
    let style = theme::dim();
    Line::from(vec![
        Span::styled(body_prefix(0), style),
        Span::styled(earlier_lines(skipped), style),
    ])
}

/// Left margin every row at nesting `depth` shares, so bodies line up.
fn indent(depth: usize) -> String {
    format!("{MESSAGE_INDENT}{}", LINE_INDENT.repeat(depth))
}

fn body_prefix(depth: usize) -> String {
    format!("{}{LINE_INDENT}", indent(depth))
}

fn marker_prefix(collapsible: &Collapsible, depth: usize) -> String {
    let marker = if collapsible.is_expanded() {
        EXPANDED_MARKER
    } else {
        COLLAPSED_MARKER
    };
    format!("{}{marker}", indent(depth))
}

pub(super) fn tail_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max {
        return text.to_string();
    }
    let skip = lines.len() - max;
    format!("{}\n{}", earlier_lines(skip), lines[skip..].join("\n"))
}

pub(super) fn wrap_row_into(
    out: &mut Vec<Line<'static>>,
    kind: LineKind,
    text: &str,
    width: usize,
) {
    if !out.is_empty() {
        out.push(Line::from(""));
    }
    if let Some(frame) = PromptFrame::of(kind)
        && width > PROMPT_FRAME_COLS + frame.marker.width()
    {
        wrap_prompt_frame_into(out, &frame, text, width);
        return;
    }
    if kind == LineKind::Separator {
        if text.is_empty() {
            out.push(Line::from(""));
        } else {
            for line in format_lines(kind, text) {
                wrap_line_into(out, &line, width, kind);
            }
        }
        return;
    }
    for line in format_lines(kind, text) {
        wrap_line_into(out, &line, width, kind);
    }
}

/// How the composer looked when a prompt was submitted: its border, prompt
/// marker and text colors.
struct PromptFrame {
    border: Style,
    marker: &'static str,
    marker_style: Style,
    body: Style,
}

impl PromptFrame {
    fn of(kind: LineKind) -> Option<Self> {
        let (border, body) = match kind {
            LineKind::User => (theme::border_idle(), Style::default()),
            LineKind::Shell => (kind.style(), kind.style()),
            _ => return None,
        };
        Some(Self {
            border,
            marker: kind.gutter(),
            marker_style: kind.style(),
            body,
        })
    }
}

/// Frames a submitted prompt like the composer, so it reads as input rather
/// than an answer. Body lines are `[left edge, marker, text, padded right edge]`;
/// the marker shows on the first line only.
fn wrap_prompt_frame_into(
    out: &mut Vec<Line<'static>>,
    frame: &PromptFrame,
    text: &str,
    width: usize,
) {
    let style = frame.border;
    let set = theme::border_type().to_border_set();
    let rule = |left: &str, fill: &str, right: &str| {
        let fill = fill.repeat(width - PROMPT_CORNER_COLS);
        Line::from(Span::styled(format!("{left}{fill}{right}"), style))
    };
    let marker_width = frame.marker.width();
    let body_width = width - PROMPT_FRAME_COLS - marker_width;
    let left_edge = format!("{} ", set.vertical_left);
    let continuation = " ".repeat(marker_width);
    let mut marker: &str = frame.marker;
    out.push(rule(set.top_left, set.horizontal_top, set.top_right));
    for part in trim_message(text).split('\n') {
        let mut rest = part.strip_suffix('\r').unwrap_or(part);
        loop {
            let (chunk, next) = split_at_width(rest, body_width);
            let pad = " ".repeat(body_width.saturating_sub(chunk.width()));
            out.push(Line::from(vec![
                Span::styled(left_edge.clone(), style),
                Span::styled(
                    mem::replace(&mut marker, &continuation).to_string(),
                    frame.marker_style,
                ),
                Span::styled(chunk.to_string(), frame.body),
                Span::styled(format!("{pad} {}", set.vertical_right), style),
            ]));
            if next.is_empty() {
                break;
            }
            rest = next;
        }
    }
    out.push(rule(
        set.bottom_left,
        set.horizontal_bottom,
        set.bottom_right,
    ));
}

pub(super) fn apply_hover(line: &Line<'static>, width: usize) -> Line<'static> {
    let hover = theme::hover();
    let mut spans: Vec<Span<'static>> = line
        .spans
        .iter()
        .map(|span| Span::styled(span.content.clone(), span.style.patch(hover)))
        .collect();
    let pad = width.saturating_sub(line_display_width(line));
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), hover));
    }
    Line::from(spans)
}

/// Wraps a collapsible row and returns its markers: the row's own header
/// first, then one per nested item that is currently rendered.
pub(super) fn wrap_collapsible_into(
    out: &mut Vec<Line<'static>>,
    kind: LineKind,
    title: &str,
    collapsible: &Collapsible,
    width: usize,
    live_rows: Option<usize>,
) -> Vec<Header> {
    if !out.is_empty() {
        out.push(Line::from(""));
    }
    let style = kind.style();
    let header = Line::from(vec![
        Span::styled(marker_prefix(collapsible, 0), style),
        Span::styled(title.to_string(), style),
    ]);
    let mut headers = vec![Header {
        line: out.len(),
        path: Vec::new(),
    }];
    wrap_line_into(out, &header, width, kind);
    if collapsible.is_expanded() {
        let body = out.len();
        let mut nested = wrap_sections_into(out, collapsible, kind, width, 0, &[]);
        if let Some(rows) = live_rows {
            window_live_body(out, &mut nested, body, rows);
        }
        headers.extend(nested);
    }
    headers
}

/// Keeps a live body within `rows` screen rows, so deltas cannot push older
/// rows out of the view. Rows dropped from the head — counted after wrapping,
/// so a single long line may cost many of them — hide behind one marker row.
fn window_live_body(
    out: &mut Vec<Line<'static>>,
    nested: &mut Vec<Header>,
    body: usize,
    rows: usize,
) {
    let total = out.len() - body;
    if total <= rows {
        return;
    }
    let skipped = total - rows + MARKER_ROWS;
    let cut = body + skipped;
    out.drain(body..cut);
    out.insert(body, earlier_lines_marker(skipped));
    nested.retain(|header| header.line >= cut);
    for header in nested {
        header.line -= skipped - MARKER_ROWS;
    }
}

fn wrap_sections_into(
    out: &mut Vec<Line<'static>>,
    collapsible: &Collapsible,
    kind: LineKind,
    width: usize,
    depth: usize,
    path: &[usize],
) -> Vec<Header> {
    let mut headers = Vec::new();
    for (idx, section) in collapsible.sections().iter().enumerate() {
        let mut path = path.to_vec();
        path.push(idx);
        match section {
            Section::Text(text) => {
                for part in text.lines() {
                    let style = kind.style();
                    let line = Line::from(vec![
                        Span::styled(body_prefix(depth), style),
                        Span::styled(part.to_string(), diff_line_style(part, style, kind)),
                    ]);
                    wrap_line_into(out, &line, width, kind);
                }
            }
            Section::Item {
                kind: item_kind,
                title,
                detail,
            } => {
                let style = item_kind.style();
                let marker = Line::from(vec![
                    Span::styled(marker_prefix(detail, depth + 1), style),
                    Span::styled(title.clone(), style),
                ]);
                let line = out.len();
                wrap_line_into(out, &marker, width, *item_kind);
                headers.push(Header {
                    line,
                    path: path.clone(),
                });
                if detail.is_expanded() {
                    headers.extend(wrap_sections_into(
                        out,
                        detail,
                        *item_kind,
                        width,
                        depth + 1,
                        &path,
                    ));
                }
            }
        }
    }
    headers
}

pub(super) fn format_lines(kind: LineKind, text: &str) -> Vec<Line<'static>> {
    if kind == LineKind::Thinking {
        let style = kind.style();
        return vec![Line::from(vec![
            Span::styled(line_prefix(kind), style),
            Span::styled(thinking_display_label(text).to_string(), style),
        ])];
    }
    let first_prefix = line_prefix(kind);
    let rest_prefix = continuation_prefix(kind, &first_prefix);
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
            &first_prefix
        } else {
            &rest_prefix
        };
        let line_style = diff_line_style(part, style, kind);
        let body = if blank {
            String::new()
        } else {
            part.to_string()
        };
        let body_span = match kind {
            LineKind::Diff | LineKind::Shell | LineKind::Separator => {
                Span::styled(body, line_style)
            }
            _ => Span::raw(body),
        };
        lines.push(Line::from(vec![
            Span::styled(head.clone(), line_style),
            body_span,
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(first_prefix, style),
            Span::raw(String::new()),
        ]));
    }
    lines
}

pub(super) fn wrap_line_into(
    out: &mut Vec<Line<'static>>,
    line: &Line<'static>,
    width: usize,
    kind: LineKind,
) {
    if width == 0 {
        out.push(line.clone());
        return;
    }
    let (prefix, style, body_style, body) = if let [head, rest @ ..] = line.spans.as_slice() {
        let body: String = rest.iter().map(|s| s.content.as_ref()).collect();
        let body_style = rest
            .first()
            .map(|span| span.style)
            .filter(|style| *style != Style::default());
        (
            head.content.as_ref().to_string(),
            head.style,
            body_style,
            body,
        )
    } else {
        out.push(line.clone());
        return;
    };
    let body_width = width.saturating_sub(prefix.width()).max(1);
    if body.is_empty() {
        out.push(Line::from(vec![
            Span::styled(prefix, style),
            body_span(String::new(), body_style),
        ]));
        return;
    }
    let continuation = continuation_prefix(kind, &prefix);
    let mut head = prefix.as_str();
    let mut rest = body.as_str();
    while !rest.is_empty() {
        let (chunk, next) = split_at_width(rest, body_width);
        out.push(Line::from(vec![
            Span::styled(head.to_string(), style),
            body_span(chunk.to_string(), body_style),
        ]));
        head = continuation.as_str();
        rest = next;
    }
}

fn line_prefix(kind: LineKind) -> String {
    let indent = match kind {
        LineKind::User => "",
        _ => MESSAGE_INDENT,
    };
    format!("{indent}{}", kind.gutter())
}

fn continuation_prefix(kind: LineKind, first_prefix: &str) -> String {
    if kind.gutter_once() {
        " ".repeat(first_prefix.width())
    } else {
        first_prefix.to_string()
    }
}

fn diff_line_style(part: &str, fallback: Style, kind: LineKind) -> Style {
    if kind != LineKind::Diff {
        return fallback;
    }
    match part.chars().next() {
        Some('+') => theme::diff_added(),
        Some('-') => theme::diff_removed(),
        _ => fallback,
    }
}

fn body_span(text: String, style: Option<Style>) -> Span<'static> {
    match style {
        Some(style) => Span::styled(text, style),
        None => Span::raw(text),
    }
}

pub(crate) fn split_at_width(s: &str, max_width: usize) -> (&str, &str) {
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

pub(super) fn line_display_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|s| s.content.width()).sum()
}

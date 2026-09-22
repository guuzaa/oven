use std::f32::consts::TAU;
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::collapsible::Collapsible;
use super::super::theme;
use super::kinds::{LINE_INDENT, LineKind, MESSAGE_INDENT, SEPARATOR_GLYPH};

pub(super) const MAX_SHELL_DISPLAY_LINES: usize = 100;
pub(super) const MAX_LIVE_BODY_LINES: usize = 8;
pub(super) const THINKING_LABEL: &str = "Thinking...";
pub(super) const THOUGHT_LABEL: &str = "Thought";
pub(super) const RESULT_LABEL: &str = "Result";
const MS_PER_SECOND: u64 = 1000;
const MS_PER_TENTH: u64 = 100;
const TENTHS_PER_SECOND: u64 = 10;
const MS_PER_MINUTE: u64 = 60_000;
const WORKED_FOR: &str = "Worked for";
const THOUGHT_FOR: &str = "Thought for";

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

pub(super) fn paint_visible(f: &mut Frame<'_>, area: Rect, lines: Vec<Line<'static>>) {
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

pub(super) fn thinking_phase() -> f32 {
    const PERIOD_MS: u128 = 1400;
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_millis() % PERIOD_MS) as f32 / PERIOD_MS as f32)
        .unwrap_or(0.0)
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
    let n = text.chars().count().max(1) as f32;
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            let wave = ((i as f32 / n - phase) * TAU).cos() * 0.5 + 0.5;
            Span::styled(ch.to_string(), Style::default().fg(thinking_shade(wave)))
        })
        .collect()
}

fn thinking_shade(t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let lo = 88.0;
    let hi = 220.0;
    let v = (lo + (hi - lo) * t) as u8;
    Color::Rgb(v, v, v)
}

fn earlier_lines(skipped: usize) -> String {
    format!("… {skipped} earlier lines")
}

fn earlier_lines_marker(skipped: usize) -> Line<'static> {
    let style = theme::dim();
    Line::from(vec![
        Span::styled(format!("{MESSAGE_INDENT}{LINE_INDENT}"), style),
        Span::styled(format!("{LINE_INDENT}{}", earlier_lines(skipped)), style),
    ])
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
    if kind == LineKind::Separator {
        if text.is_empty() {
            out.push(separator_line(width));
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

fn separator_line(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        SEPARATOR_GLYPH.to_string().repeat(width.max(1)),
        theme::dim(),
    ))
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

pub(super) fn wrap_collapsible_into(
    out: &mut Vec<Line<'static>>,
    kind: LineKind,
    title: &str,
    collapsible: &Collapsible,
    width: usize,
    live_limit: Option<usize>,
) {
    if !out.is_empty() {
        out.push(Line::from(""));
    }
    let style = kind.style();
    let marker = if collapsible.is_expanded() {
        "⌄ "
    } else {
        "› "
    };
    let header = Line::from(vec![
        Span::styled(kind.gutter().to_string(), style),
        Span::styled(format!("{marker}{title}"), style),
    ]);
    wrap_line_into(out, &header, width, kind);
    if !collapsible.is_expanded() {
        return;
    }
    let (skipped, body_lines) = collapsible.visible_body(live_limit);
    if skipped > 0 {
        wrap_line_into(out, &earlier_lines_marker(skipped), width, kind);
    }
    for part in body_lines {
        let body_style = if kind == LineKind::Diff {
            diff_line_style(part, style)
        } else {
            style
        };
        let line = Line::from(vec![
            Span::styled(format!("{MESSAGE_INDENT}{LINE_INDENT}"), style),
            Span::styled(format!("{LINE_INDENT}{part}"), body_style),
        ]);
        wrap_line_into(out, &line, width, kind);
    }
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
        let line_style = if kind == LineKind::Diff {
            diff_line_style(part, style)
        } else {
            style
        };
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
    let (prefix, style, body_style, body) = match line.spans.as_slice() {
        [head, rest @ ..] => {
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
        }
        [] => {
            out.push(line.clone());
            return;
        }
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

fn diff_line_style(part: &str, fallback: Style) -> Style {
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

pub(super) fn split_at_width(s: &str, max_width: usize) -> (&str, &str) {
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

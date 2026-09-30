use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::kinds::LINE_PREFIX_WIDTH;
use crate::core::theme;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct SelPos {
    pub line: usize,
    pub col: usize,
}

/// Gutter width and body of a rendered line: framed prompt lines are
/// `[edge, marker, body, edge]`, every other line leads with its gutter.
fn line_parts(line: &Line<'_>) -> (usize, String) {
    match line.spans.as_slice() {
        [edge, marker, body, _] => (
            edge.content.width() + marker.content.width(),
            body.content.to_string(),
        ),
        [head, rest @ ..] if head.content.width() >= LINE_PREFIX_WIDTH => {
            (head.content.width(), join_content(rest))
        }
        spans => (0, join_content(spans)),
    }
}

fn join_content(spans: &[Span<'_>]) -> String {
    spans.iter().map(|s| s.content.as_ref()).collect()
}

pub(super) fn extract_line_range(line: &Line<'_>, from_col: usize, to_col: usize) -> String {
    let (prefix, body) = line_parts(line);
    let from = from_col.saturating_sub(prefix).min(body.width());
    let to = to_col.saturating_sub(prefix).min(body.width());
    slice_cols(&body, from, to)
}

pub(super) fn slice_cols(s: &str, start: usize, end: usize) -> String {
    let mut out = String::new();
    if start >= end {
        return out;
    }

    let mut col = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if col + cw > start && col < end {
            out.push(ch);
        }
        col += cw;
        if col >= end {
            break;
        }
    }
    out
}

pub(super) fn highlight_line(
    line: &Line<'static>,
    from_col: usize,
    to_col: usize,
) -> Line<'static> {
    if from_col >= to_col {
        return line.clone();
    }
    let style = theme::selection();
    let mut col = 0;
    let mut spans = Vec::new();
    for span in &line.spans {
        let text = span.content.as_ref();
        let w = text.width();
        let span_start = col;
        let span_end = col + w;
        col = span_end;
        if w == 0 || span_end <= from_col || span_start >= to_col {
            spans.push(span.clone());
            continue;
        }
        let mut before = String::new();
        let mut mid = String::new();
        let mut after = String::new();
        let mut x = span_start;
        for ch in text.chars() {
            let cw = ch.width().unwrap_or(0);
            if x + cw <= from_col {
                before.push(ch);
            } else if x >= to_col {
                after.push(ch);
            } else {
                mid.push(ch);
            }
            x += cw;
        }
        if !before.is_empty() {
            spans.push(Span::styled(before, span.style));
        }
        if !mid.is_empty() {
            spans.push(Span::styled(mid, style));
        }
        if !after.is_empty() {
            spans.push(Span::styled(after, span.style));
        }
    }
    Line::from(spans)
}

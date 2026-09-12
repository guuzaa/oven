use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::theme;
use super::kinds::LINE_PREFIX_WIDTH;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct SelPos {
    pub line: usize,
    pub col: usize,
}

fn line_prefix_width(line: &Line<'_>) -> usize {
    match line.spans.as_slice() {
        [head, rest @ ..] if !rest.is_empty() && head.content.width() == LINE_PREFIX_WIDTH => {
            LINE_PREFIX_WIDTH
        }
        _ => 0,
    }
}

fn line_body(line: &Line<'_>) -> String {
    match line.spans.as_slice() {
        [head, rest @ ..] if head.content.width() == LINE_PREFIX_WIDTH => {
            rest.iter().map(|s| s.content.as_ref()).collect()
        }
        spans => spans.iter().map(|s| s.content.as_ref()).collect(),
    }
}

pub(super) fn extract_line_range(line: &Line<'_>, from_col: usize, to_col: usize) -> String {
    let prefix = line_prefix_width(line);
    let body = line_body(line);
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

#[cfg(not(test))]
pub(super) fn copy_to_clipboard(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_text(text))
        .is_ok()
        || osc52_copy(text)
}

#[cfg(test)]
pub(super) fn copy_to_clipboard(_text: &str) -> bool {
    true
}

#[cfg(not(test))]
fn osc52_copy(text: &str) -> bool {
    use std::io::{self, Write};

    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    let encoded = STANDARD.encode(text.as_bytes());
    write!(io::stdout(), "\x1b]52;c;{encoded}\x07")
        .and_then(|_| io::stdout().flush())
        .is_ok()
}

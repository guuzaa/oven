use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::status::truncate_str;

/// Maximum number of queued messages shown in the widget; the rest are
/// summarized in a footer row.
const MAX_VISIBLE: usize = 3;

/// Renders messages queued while the app is busy, one row per message.
pub struct QueueWidget;

impl QueueWidget {
    pub fn new() -> Self {
        Self
    }

    /// Height of the widget, or 0 when there is nothing queued.
    pub fn height(&self, pending: &[String]) -> u16 {
        if pending.is_empty() {
            return 0;
        }
        let rows = pending.len().min(MAX_VISIBLE) + usize::from(pending.len() > MAX_VISIBLE);
        (rows + 2) as u16 // + borders
    }

    pub fn draw(&mut self, f: &mut Frame<'_>, area: Rect, pending: &[String]) {
        if pending.is_empty() {
            return;
        }
        let inner_width = area.width.saturating_sub(2) as usize;
        let mut lines = Vec::with_capacity(MAX_VISIBLE + 1);
        for (i, text) in pending.iter().take(MAX_VISIBLE).enumerate() {
            lines.push(preview_line(text, i + 1, inner_width));
        }
        if pending.len() > MAX_VISIBLE {
            let more = pending.len() - MAX_VISIBLE;
            lines.push(Line::from(Span::styled(
                format!("… and {more} more queued"),
                Style::default().fg(Color::DarkGray),
            )));
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" queued ({}) ", pending.len()));
        f.render_widget(Paragraph::new(lines).block(block), area);
    }
}

/// One row: a numbered prefix plus the message preview.
fn preview_line(text: &str, index: usize, inner_width: usize) -> Line<'static> {
    let mut preview = text.lines().next().unwrap_or("").to_string();
    if text.lines().count() > 1 {
        preview.push('…');
    }
    preview = truncate_str(&preview, inner_width.saturating_sub(3));
    Line::from(vec![
        Span::styled(format!("{:>2} ", index), Style::default().fg(Color::Cyan)),
        Span::raw(preview),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    fn q(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn height_is_zero_when_empty() {
        let widget = QueueWidget::new();
        assert_eq!(widget.height(&[]), 0);
    }

    #[test]
    fn height_grows_with_messages() {
        let widget = QueueWidget::new();
        assert_eq!(widget.height(&q(&["a"])), 3);
        assert_eq!(widget.height(&q(&["a", "b", "c"])), 5);
    }

    #[test]
    fn height_caps_at_max_visible_with_footer() {
        let widget = QueueWidget::new();
        assert_eq!(widget.height(&q(&["a", "b", "c", "d"])), 6);
    }

    #[test]
    fn preview_numbers_rows() {
        let line = preview_line("hello", 2, 40);
        assert_eq!(line.spans[0].content.as_ref(), " 2 ");
    }

    #[test]
    fn preview_uses_first_line_of_multiline_message() {
        let line = preview_line("first line\nsecond line", 1, 40);
        assert_eq!(line.spans[1].content.as_ref(), "first line…");
    }

    #[test]
    fn preview_truncates_long_lines() {
        let line = preview_line(&"x".repeat(100), 1, 20);
        let preview = line.spans[1].content.as_ref();
        assert!(preview.ends_with('…'));
        assert!(preview.width() <= 17);
    }
}

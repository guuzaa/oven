use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::list::truncate_str;
use crate::core::theme;

/// Row the queued-message banner occupies, zero when nothing is queued.
pub fn height(count: usize) -> u16 {
    u16::from(count > 0)
}

/// Renders a single compact row of messages queued while the app is busy.
/// `extra` is how many messages follow `first`.
pub fn draw(f: &mut Frame<'_>, area: Rect, first: &str, extra: usize) {
    let inner_width = area.width as usize;
    let first = first.lines().next().unwrap_or("");
    let extra = if extra > 0 {
        format!("  +{extra}")
    } else {
        String::new()
    };
    let budget = inner_width.saturating_sub("queued · ".len() + extra.len());
    let preview = truncate_str(first, budget);
    let text = format!("queued · {preview}{extra}");
    f.render_widget(Paragraph::new(Span::styled(text, theme::dim())), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_is_zero_when_empty() {
        assert_eq!(height(0), 0);
    }

    #[test]
    fn height_is_one_when_queued() {
        assert_eq!(height(1), 1);
        assert_eq!(height(4), 1);
    }
}

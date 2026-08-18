use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::status::truncate_str;
use super::theme;

/// Renders a single compact row of messages queued while the app is busy.
pub struct QueueWidget;

impl QueueWidget {
    pub fn new() -> Self {
        Self
    }

    pub fn height(&self, pending: &[String]) -> u16 {
        u16::from(!pending.is_empty())
    }

    pub fn draw(&mut self, f: &mut Frame<'_>, area: Rect, pending: &[String]) {
        if pending.is_empty() {
            return;
        }
        let inner_width = area.width as usize;
        let first = pending[0].lines().next().unwrap_or("");
        let extra = if pending.len() > 1 {
            format!("  +{}", pending.len() - 1)
        } else {
            String::new()
        };
        let budget = inner_width.saturating_sub("queued · ".len() + extra.len());
        let preview = truncate_str(first, budget);
        let text = format!("queued · {preview}{extra}");
        f.render_widget(Paragraph::new(Span::styled(text, theme::dim())), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn height_is_zero_when_empty() {
        let widget = QueueWidget::new();
        assert_eq!(widget.height(&[]), 0);
    }

    #[test]
    fn height_is_one_when_queued() {
        let widget = QueueWidget::new();
        assert_eq!(widget.height(&q(&["a"])), 1);
        assert_eq!(widget.height(&q(&["a", "b", "c", "d"])), 1);
    }
}

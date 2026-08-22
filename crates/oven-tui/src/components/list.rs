use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::theme;

pub const MAX_LIST_ROWS: usize = 6;
const SELECTED_MARK: &str = "▸ ";
const IDLE_MARK: &str = "  ";

pub fn cycle_selected(selected: &mut usize, n: usize, up: bool) {
    if n == 0 {
        return;
    }
    *selected = if up {
        (*selected + n - 1) % n
    } else {
        (*selected + 1) % n
    };
}

pub fn draw_choice_list<N, D>(
    f: &mut Frame<'_>,
    area: Rect,
    items: impl IntoIterator<Item = (N, D)>,
    selected: usize,
) where
    N: Into<String>,
    D: Into<String>,
{
    let mut lines = Vec::new();
    for (row, (name, desc)) in items.into_iter().enumerate() {
        let name_style = if row == selected {
            theme::accent()
        } else {
            Style::default()
        };
        let mark = if row == selected {
            SELECTED_MARK
        } else {
            IDLE_MARK
        };
        lines.push(Line::from(vec![
            Span::styled(mark.to_string(), name_style),
            Span::styled(name.into(), name_style),
            Span::styled(format!("  {}", desc.into()), theme::dim()),
        ]));
    }
    f.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_wraps_both_directions() {
        let mut i = 0;
        cycle_selected(&mut i, 3, false);
        assert_eq!(i, 1);
        cycle_selected(&mut i, 3, false);
        cycle_selected(&mut i, 3, false);
        assert_eq!(i, 0);
        cycle_selected(&mut i, 3, true);
        assert_eq!(i, 2);
        cycle_selected(&mut i, 0, false);
        assert_eq!(i, 2);
    }

    #[test]
    fn selected_row_uses_marker_prefix() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(24, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_choice_list(f, f.area(), [("exit", "leave"), ("clear", "wipe")], 1);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let row0: String = (0..24).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        let row1: String = (0..24).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(row0.contains("  exit"), "{row0:?}");
        assert!(row1.contains("▸ clear"), "{row1:?}");
        assert!(!row0.contains("▸"), "{row0:?}");
    }
}

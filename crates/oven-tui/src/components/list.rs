use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::theme;

pub const MAX_LIST_ROWS: usize = 6;
const SELECTED_MARK: &str = "▸ ";
const IDLE_MARK: &str = "  ";
const TITLE_ROWS: u16 = 2;

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

pub fn titled_list_height(item_count: usize) -> u16 {
    TITLE_ROWS.saturating_add(u16::try_from(item_count).unwrap_or(u16::MAX))
}

/// Rows a bounded list needs for `count` entries: at least one, at most
/// [`MAX_LIST_ROWS`].
pub fn bounded_rows(count: usize) -> u16 {
    u16::try_from(count.clamp(1, MAX_LIST_ROWS)).unwrap_or(u16::MAX)
}

pub fn draw_titled_choice_list<N, D>(
    f: &mut Frame<'_>,
    area: Rect,
    title: &str,
    detail: &str,
    items: impl IntoIterator<Item = (N, D)>,
    selected: usize,
) where
    N: Into<String>,
    D: Into<String>,
{
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(TITLE_ROWS), Constraint::Min(0)])
        .split(area);
    f.render_widget(Paragraph::new(format!("{title}\n{detail}")), rows[0]);
    draw_choice_list(f, rows[1], items, selected);
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

    #[test]
    fn titled_list_shows_heading_then_choices() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        assert_eq!(titled_list_height(2), TITLE_ROWS + 2);
        let backend = TestBackend::new(32, titled_list_height(2));
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_titled_choice_list(
                    f,
                    f.area(),
                    "limit reached",
                    "ran 2 iterations",
                    [("Continue", "keep going"), ("Exit", "stop")],
                    0,
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let row = |y| {
            (0..32)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        };
        assert!(row(0).contains("limit reached"), "{:?}", row(0));
        assert!(row(1).contains("ran 2 iterations"), "{:?}", row(1));
        assert!(row(2).contains("▸ Continue"), "{:?}", row(2));
        assert!(row(3).contains("  Exit"), "{:?}", row(3));
    }
}

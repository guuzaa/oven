use oven_app::{AgentEvent, AppEvent, TodoList, TodoStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::status::truncate_str;
use super::theme;

const MAX_HEIGHT: u16 = 6;

/// Read-only checklist of the current session TODO list.
pub struct TodosWidget {
    list: TodoList,
}

impl TodosWidget {
    pub fn new(list: TodoList) -> Self {
        Self { list }
    }

    pub fn height(&self) -> u16 {
        if self.list.is_empty() {
            0
        } else {
            (self.list.items.len() as u16).min(MAX_HEIGHT)
        }
    }

    pub fn on_event(&mut self, ev: &AppEvent) {
        let AppEvent::Agent { event, .. } = ev else {
            return;
        };
        match event {
            AgentEvent::TodoUpdated { items, .. } => {
                self.list.items = items.clone();
            }
            AgentEvent::HistoryCleared { .. } => {
                self.list.items.clear();
            }
            _ => {}
        }
    }

    pub fn draw(&self, f: &mut Frame<'_>, area: Rect) {
        if self.list.is_empty() || area.height == 0 {
            return;
        }
        let width = area.width as usize;
        let n = (area.height as usize).min(self.list.items.len());
        let lines: Vec<Line> = self.list.items[..n]
            .iter()
            .map(|item| item_line(item.status, &item.content, width))
            .collect();
        f.render_widget(Paragraph::new(lines), area);
    }
}

fn item_line(status: TodoStatus, content: &str, width: usize) -> Line<'static> {
    let (mark, style) = match status {
        TodoStatus::Pending => ("[ ] ", Style::default()),
        TodoStatus::InProgress => ("[~] ", theme::accent()),
        TodoStatus::Completed => ("[x] ", theme::dim()),
        TodoStatus::Cancelled => ("[-] ", theme::dim()),
    };
    let first = content.lines().next().unwrap_or("");
    let preview = truncate_str(first, width.saturating_sub(mark.len()));
    Line::from(vec![
        Span::styled(mark.to_string(), style),
        Span::styled(preview, style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use oven_app::{AgentId, AppId, TodoItem};

    fn item(id: &str, content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            id: id.into(),
            content: content.into(),
            status,
        }
    }

    fn list(items: Vec<TodoItem>) -> TodoList {
        TodoList { items }
    }

    fn sample() -> TodoList {
        list(vec![
            item("a", "pending task", TodoStatus::Pending),
            item("b", "active task", TodoStatus::InProgress),
            item("c", "done task", TodoStatus::Completed),
        ])
    }

    fn agent_event(event: AgentEvent) -> AppEvent {
        AppEvent::Agent {
            app_id: AppId(1),
            event,
        }
    }

    fn render(widget: &TodosWidget) -> String {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let h = widget.height().max(1);
        let backend = TestBackend::new(40, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                widget.draw(f, f.area());
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..h {
            for x in 0..40 {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn empty_list_has_zero_height() {
        let widget = TodosWidget::new(TodoList::default());
        assert_eq!(widget.height(), 0);
    }

    #[test]
    fn non_empty_renders_checklist_marks() {
        let widget = TodosWidget::new(sample());
        assert_eq!(widget.height(), 3);
        let row = render(&widget);
        assert!(row.contains("[ ]"), "pending mark missing: {row:?}");
        assert!(row.contains("[~]"), "in_progress mark missing: {row:?}");
        assert!(row.contains("[x]"), "completed mark missing: {row:?}");
        assert!(row.contains("pending task"), "{row:?}");
        assert!(row.contains("active task"), "{row:?}");
        assert!(row.contains("done task"), "{row:?}");
    }

    #[test]
    fn todo_updated_empty_hides_widget() {
        let mut widget = TodosWidget::new(sample());
        assert_eq!(widget.height(), 3);
        widget.on_event(&agent_event(AgentEvent::TodoUpdated {
            agent_id: AgentId(1),
            items: Vec::new(),
        }));
        assert_eq!(widget.height(), 0);
    }

    #[test]
    fn history_cleared_hides_widget() {
        let mut widget = TodosWidget::new(sample());
        widget.on_event(&agent_event(AgentEvent::HistoryCleared {
            agent_id: AgentId(1),
        }));
        assert_eq!(widget.height(), 0);
    }

    #[test]
    fn resume_snapshot_shows_list_without_event() {
        let widget = TodosWidget::new(sample());
        assert_eq!(widget.height(), 3);
        let row = render(&widget);
        assert!(row.contains("[ ]"));
        assert!(row.contains("[~]"));
        assert!(row.contains("[x]"));
    }

    #[test]
    fn height_caps_at_six() {
        let items = (0..10)
            .map(|i| item(&format!("t{i}"), &format!("task {i}"), TodoStatus::Pending))
            .collect();
        let widget = TodosWidget::new(list(items));
        assert_eq!(widget.height(), 6);
    }

    #[test]
    fn cancelled_uses_dash_mark() {
        let widget = TodosWidget::new(list(vec![item("x", "dropped", TodoStatus::Cancelled)]));
        let row = render(&widget);
        assert!(row.contains("[-]"), "{row:?}");
        assert!(row.contains("dropped"), "{row:?}");
    }
}

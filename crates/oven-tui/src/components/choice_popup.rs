use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;

use super::list;

const APPROVAL_TITLE: &str = "permission required";
const APPROVAL_ITEMS: [(&str, &str); 2] = [
    ("Approve", "run this tool"),
    ("Reject", "do not run this tool"),
];

const LOOP_LIMIT_TITLE: &str = "agent loop limit reached";
const LOOP_LIMIT_ITEMS: [(&str, &str); 2] = [
    ("Continue", "run another round"),
    ("Exit", "stop this turn"),
];

pub(crate) enum ChoicePopupAction {
    Handled,
    Confirm(usize),
    Cancel,
}

pub(crate) struct ChoicePopup {
    title: String,
    detail: String,
    items: &'static [(&'static str, &'static str)],
    selected: usize,
}

impl ChoicePopup {
    pub(crate) fn approval(name: &str, summary: &str) -> Self {
        Self {
            title: format!("{APPROVAL_TITLE} · {name}"),
            detail: summary.to_string(),
            items: &APPROVAL_ITEMS,
            selected: 0,
        }
    }

    pub(crate) fn loop_limit(max_iters: usize) -> Self {
        Self {
            title: LOOP_LIMIT_TITLE.to_string(),
            detail: format!("ran {max_iters} iterations"),
            items: &LOOP_LIMIT_ITEMS,
            selected: 0,
        }
    }

    pub(crate) fn height(&self) -> u16 {
        list::titled_list_height(self.items.len())
    }

    pub(crate) fn draw(&self, f: &mut Frame<'_>, area: Rect) {
        list::draw_titled_choice_list(
            f,
            area,
            &self.title,
            &self.detail,
            self.items.iter().copied(),
            self.selected,
        );
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ChoicePopupAction {
        match key.code {
            KeyCode::Up => {
                list::cycle_selected(&mut self.selected, self.items.len(), true);
                ChoicePopupAction::Handled
            }
            KeyCode::Down => {
                list::cycle_selected(&mut self.selected, self.items.len(), false);
                ChoicePopupAction::Handled
            }
            KeyCode::Enter if key.modifiers.is_empty() => ChoicePopupAction::Confirm(self.selected),
            KeyCode::Char('y') => ChoicePopupAction::Confirm(0),
            KeyCode::Esc | KeyCode::Char('n') => {
                ChoicePopupAction::Confirm(self.items.len().saturating_sub(1))
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                ChoicePopupAction::Cancel
            }
            _ => ChoicePopupAction::Handled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn enter_confirms_selected_row() {
        let mut popup = ChoicePopup::loop_limit(100);
        assert!(matches!(
            popup.handle_key(key(KeyCode::Enter)),
            ChoicePopupAction::Confirm(0)
        ));
        let mut popup = ChoicePopup::loop_limit(100);
        popup.handle_key(key(KeyCode::Down));
        assert!(matches!(
            popup.handle_key(key(KeyCode::Enter)),
            ChoicePopupAction::Confirm(1)
        ));
    }

    #[test]
    fn y_continues_n_and_esc_exit() {
        let mut popup = ChoicePopup::loop_limit(2);
        assert!(matches!(
            popup.handle_key(key(KeyCode::Char('y'))),
            ChoicePopupAction::Confirm(0)
        ));
        let mut popup = ChoicePopup::loop_limit(2);
        assert!(matches!(
            popup.handle_key(key(KeyCode::Char('n'))),
            ChoicePopupAction::Confirm(1)
        ));
        let mut popup = ChoicePopup::loop_limit(2);
        assert!(matches!(
            popup.handle_key(key(KeyCode::Esc)),
            ChoicePopupAction::Confirm(1)
        ));
    }

    #[test]
    fn ctrl_c_cancels() {
        let mut popup = ChoicePopup::approval("bash", "run ls");
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(matches!(popup.handle_key(key), ChoicePopupAction::Cancel));
    }
}

//! The keys the frontend reads on its own, before a widget sees them.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Shift-Tab, however the terminal reports it: the key that flips the mode.
pub fn is_mode_toggle(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::BackTab)
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn is_mode_toggle_backtab_and_shift_tab() {
        assert!(is_mode_toggle(key(KeyCode::BackTab, KeyModifiers::NONE)));
        assert!(is_mode_toggle(key(KeyCode::Tab, KeyModifiers::SHIFT)));
        assert!(!is_mode_toggle(key(KeyCode::Tab, KeyModifiers::NONE)));
        assert!(!is_mode_toggle(key(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
    }
}

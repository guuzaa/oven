//! The keys that apply right now, as the composer border states them.

pub const IDLE: &str = "enter send · shift-tab mode · esc undo";
pub const VIEWER: &str = "esc back to the chat · ↑↓ scroll · x stop";
pub const BUSY: &str = "esc cancel · enter queue";
pub const ESC_ARMED: &str = "esc again to confirm";
pub const ANSWER: &str = "enter send · esc back";

/// What an open overlay prompt states on the composer border.
pub enum Prompt {
    /// The prompt names its own keys.
    Keys(&'static str),
    /// The prompt expects its answer typed into the composer, which therefore
    /// states the keys.
    Answer,
}

/// Whichever box owns the keyboard states its keys, and the composer falls
/// back to its own: an armed `Esc` outranks a busy turn, which outranks idle.
pub fn composer(
    overlay: Option<&'static str>,
    prompt: Option<Prompt>,
    busy: bool,
    esc_armed: bool,
) -> Option<&'static str> {
    if let Some(prompt) = prompt {
        return Some(match prompt {
            Prompt::Keys(hint) => hint,
            Prompt::Answer => ANSWER,
        });
    }
    if let Some(hint) = overlay {
        return Some(hint);
    }
    Some(if esc_armed {
        ESC_ARMED
    } else if busy {
        BUSY
    } else {
        IDLE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_hint_follows_focus_then_state() {
        assert_eq!(composer(None, None, true, false), Some(BUSY));
        assert_eq!(composer(None, None, false, false), Some(IDLE));
        assert_eq!(
            composer(None, None, false, true),
            Some(ESC_ARMED),
            "the armed Esc overrides the idle hint"
        );
        assert_eq!(composer(Some("tab fill"), None, false, true), Some("tab fill"));
    }

    #[test]
    fn an_open_prompt_states_its_own_keys() {
        assert_eq!(
            composer(None, Some(Prompt::Keys("enter")), false, true),
            Some("enter")
        );
        assert_eq!(
            composer(Some("tab fill"), Some(Prompt::Answer), true, true),
            Some(ANSWER),
            "the composer answers for a typed answer, whatever else is open"
        );
    }
}

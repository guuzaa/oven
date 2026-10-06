//! The keys that apply right now, as the composer border states them.

pub const IDLE: &str = "enter send · shift-tab mode · esc undo";
pub const VIEWER: &str = "↑↓ switch · esc back · pgup/pgdn scroll · x stop";
/// Arrows move a highlight. Enter is not live until one lands.
pub const STRIP_SELECT: &str = "↑↓ select · esc undo";
pub const STRIP_SELECT_BUSY: &str = "↑↓ select · esc cancel";
pub const STRIP: &str = "↑↓ select · enter view · esc undo";
pub const STRIP_BUSY: &str = "↑↓ select · enter view · esc cancel";
pub const STRIP_DONE: &str = "↑↓ select · enter view · esc close";
pub const BUSY: &str = "esc cancel · enter queue";
pub const ESC_ARMED: &str = "esc again to confirm";
pub const ANSWER: &str = "enter send · esc back";

/// What an open overlay prompt states on the composer border.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prompt {
    /// The prompt names its own keys.
    Keys(&'static str),
    /// The prompt expects its answer typed into the composer, which therefore
    /// states the keys.
    Answer,
}

/// Whether the subagent strip is taking ↑↓ and Enter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strip {
    /// The strip is not taking keys.
    Off,
    /// Arrows move a highlight. Enter does nothing until one lands.
    Select,
    /// A row is highlighted, so Enter opens it. `settled` means nothing is
    /// still running and Esc is not doing something else, so the hint offers
    /// to hide the strip.
    Open { settled: bool },
}

/// The keys the composer border states, highest rank first: an open prompt,
/// an overlay, an armed `Esc`, the subagent strip, a busy turn, then idle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keys {
    pub overlay: Option<&'static str>,
    pub prompt: Option<Prompt>,
    pub busy: bool,
    pub esc_armed: bool,
    pub strip: Strip,
}

impl Keys {
    pub fn resting(busy: bool, esc_armed: bool, strip: Strip) -> Self {
        Self {
            overlay: None,
            prompt: None,
            busy,
            esc_armed,
            strip,
        }
    }
}

pub fn composer(keys: Keys) -> Option<&'static str> {
    if let Some(prompt) = keys.prompt {
        return Some(match prompt {
            Prompt::Keys(hint) => hint,
            Prompt::Answer => ANSWER,
        });
    }
    if let Some(hint) = keys.overlay {
        return Some(hint);
    }
    if keys.esc_armed {
        return Some(ESC_ARMED);
    }
    let strip = match keys.strip {
        Strip::Off => None,
        Strip::Select => Some(if keys.busy {
            STRIP_SELECT_BUSY
        } else {
            STRIP_SELECT
        }),
        Strip::Open { settled } => Some(match (keys.busy, settled) {
            (true, _) => STRIP_BUSY,
            (false, true) => STRIP_DONE,
            (false, false) => STRIP,
        }),
    };
    if let Some(hint) = strip {
        return Some(hint);
    }
    Some(if keys.busy { BUSY } else { IDLE })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(busy: bool, esc_armed: bool, strip: Strip) -> Keys {
        Keys::resting(busy, esc_armed, strip)
    }

    #[test]
    fn composer_hint_follows_focus_then_state() {
        assert_eq!(composer(keys(true, false, Strip::Off)), Some(BUSY));
        assert_eq!(composer(keys(false, false, Strip::Off)), Some(IDLE));
        assert_eq!(
            composer(keys(false, true, Strip::Open { settled: false })),
            Some(ESC_ARMED),
            "the armed Esc overrides the strip hint"
        );
        assert_eq!(
            composer(Keys {
                overlay: Some("tab fill"),
                ..keys(false, true, Strip::Open { settled: true })
            }),
            Some("tab fill")
        );
        assert_eq!(
            composer(keys(false, false, Strip::Select)),
            Some(STRIP_SELECT),
            "enter is not offered until a row is highlighted"
        );
        assert!(!STRIP_SELECT.contains("enter view"));
        assert_eq!(
            composer(keys(true, false, Strip::Select)),
            Some(STRIP_SELECT_BUSY)
        );
        assert_eq!(
            composer(keys(false, false, Strip::Open { settled: false })),
            Some(STRIP)
        );
        assert_eq!(
            composer(keys(true, false, Strip::Open { settled: true })),
            Some(STRIP_BUSY)
        );
        assert_eq!(
            composer(keys(false, false, Strip::Open { settled: true })),
            Some(STRIP_DONE)
        );
    }

    #[test]
    fn an_open_prompt_states_its_own_keys() {
        assert_eq!(
            composer(Keys {
                prompt: Some(Prompt::Keys("enter")),
                ..keys(false, true, Strip::Open { settled: true })
            }),
            Some("enter")
        );
        assert_eq!(
            composer(Keys {
                overlay: Some("tab fill"),
                prompt: Some(Prompt::Answer),
                ..keys(true, true, Strip::Open { settled: true })
            }),
            Some(ANSWER),
            "the composer answers for a typed answer, whatever else is open"
        );
    }
}

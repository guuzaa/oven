//! The overloaded `Esc` key: which action the screen is offering, and whether
//! it waits for a second press.

use std::time::Duration;

/// `Esc` only acts when it is pressed twice inside this window, so a stray
/// press cannot cancel a turn or rewind the transcript.
pub const ESC_CONFIRM_WINDOW: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub enum EscAction {
    PopQueue,
    CloseViewer,
    Cancel,
    Rewind,
    Ignore,
}

impl EscAction {
    /// Whether the action happens on the first press. Everything that throws
    /// work away waits for a second one; leaving a subagent's transcript
    /// throws nothing away — the view is still there to reopen — and a screen
    /// that ignores the first `Esc` reads as one you are stuck on.
    pub fn acts_immediately(&self) -> bool {
        matches!(self, Self::CloseViewer)
    }

    pub fn new(
        queued: Option<&str>,
        focused: bool,
        busy: bool,
        rewinding: bool,
        last_user: Option<&str>,
    ) -> Self {
        if queued.is_some() {
            return EscAction::PopQueue;
        }
        if focused {
            return EscAction::CloseViewer;
        }
        if busy {
            return EscAction::Cancel;
        }
        if rewinding {
            return EscAction::Ignore;
        }
        if last_user.is_some() {
            return EscAction::Rewind;
        }
        EscAction::Ignore
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_action_priority_queue_then_viewer_then_cancel_then_rewind() {
        assert!(matches!(
            EscAction::new(Some("q"), true, true, false, Some("u")),
            EscAction::PopQueue
        ));
        assert!(matches!(
            EscAction::new(Some("q"), true, true, true, None),
            EscAction::PopQueue
        ));
        assert!(matches!(
            EscAction::new(None, true, true, false, Some("u")),
            EscAction::CloseViewer
        ));
        assert!(matches!(
            EscAction::new(None, false, true, false, Some("u")),
            EscAction::Cancel
        ));
        assert!(matches!(
            EscAction::new(None, false, false, true, Some("u")),
            EscAction::Ignore
        ));
        assert!(matches!(
            EscAction::new(None, false, false, false, Some("u")),
            EscAction::Rewind
        ));
        assert!(matches!(
            EscAction::new(None, false, false, false, None),
            EscAction::Ignore
        ));
    }

    #[test]
    fn only_leaving_a_view_acts_on_the_first_press() {
        assert!(EscAction::CloseViewer.acts_immediately());
        for action in [
            EscAction::PopQueue,
            EscAction::Cancel,
            EscAction::Rewind,
            EscAction::Ignore,
        ] {
            assert!(
                !action.acts_immediately(),
                "{action:?} throws work away and must be confirmed"
            );
        }
    }

    #[test]
    fn empty_prompt_cannot_trigger_rewind() {
        assert!(matches!(
            EscAction::new(None, false, false, false, None),
            EscAction::Ignore
        ));
    }
}

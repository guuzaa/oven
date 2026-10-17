//! What reached the running turn: chats typed while it worked, and notices
//! about things that happened on their own.
//!
//! One queue, several producers, one consumer. The turn takes everything
//! once, after it has written its tool results, so a notice that arrives
//! mid-step rides along with the next provider request instead of
//! interrupting it. What is still here when the turn ends reaches the next
//! one: a subagent that outlives its turn is not lost with it.

use std::sync::{Mutex, MutexGuard, PoisonError};

use oven_agent::{PendingInbox, PendingInput};

#[derive(Debug, Default)]
pub(crate) struct TurnInbox {
    items: Mutex<Vec<PendingInput>>,
}

impl TurnInbox {
    pub(crate) fn push(&self, input: PendingInput) {
        self.items().push(input);
    }

    /// Drops one parked chat equal to `text`, preferring the latest.
    /// `false` when the running turn already took it.
    pub(crate) fn claim_chat(&self, text: &str) -> bool {
        let mut items = self.items();
        let Some(index) = items
            .iter()
            .rposition(|item| matches!(item, PendingInput::User(queued) if queued == text))
        else {
            return false;
        };
        items.remove(index);
        true
    }

    fn items(&self) -> MutexGuard<'_, Vec<PendingInput>> {
        self.items.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl PendingInbox for TurnInbox {
    fn take(&self) -> Vec<PendingInput> {
        std::mem::take(&mut *self.items())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTICE: &str = "explore#1: done · 3 steps, 6 tool calls · 1040 tokens · 32.4s";

    #[test]
    fn a_take_empties_the_inbox() {
        let inbox = TurnInbox::default();
        inbox.push(PendingInput::User("hello".into()));
        inbox.push(PendingInput::Notice {
            text: NOTICE.into(),
        });
        assert_eq!(inbox.take().len(), 2);
        assert!(inbox.take().is_empty(), "a second take has nothing left");
    }

    #[test]
    fn claiming_a_chat_prefers_the_latest_one() {
        let inbox = TurnInbox::default();
        inbox.push(PendingInput::User("hello".into()));
        inbox.push(PendingInput::Notice {
            text: NOTICE.into(),
        });
        inbox.push(PendingInput::User("hello".into()));

        assert!(inbox.claim_chat("hello"));
        let left = inbox.take();
        assert_eq!(left.len(), 2, "the older chat and the notice stay");
        assert!(matches!(&left[0], PendingInput::User(text) if text == "hello"));
        assert!(
            matches!(&left[1], PendingInput::Notice { text } if text == NOTICE),
            "a notice is never claimed as a chat"
        );
    }

    #[test]
    fn a_notice_keeps_its_shape() {
        let inbox = TurnInbox::default();
        inbox.push(PendingInput::Notice {
            text: NOTICE.into(),
        });
        let taken = inbox.take();
        assert!(
            matches!(taken.as_slice(), [PendingInput::Notice { text }] if text == NOTICE),
            "{taken:?}"
        );
    }
}

//! What reached the running turn: chats typed while it worked, and notices
//! about things that happened on their own.
//!
//! One queue, several producers, one consumer. The turn takes everything
//! once, after it has written its tool results, so a notice that arrives
//! mid-step rides along with the next provider request instead of
//! interrupting it. What is still here when the turn ends reaches the next
//! one: a subagent that outlives its turn is not lost with it, and a notice
//! that asks to wake starts that next turn when nobody else does.
//!
//! A chat is the one thing a tool may stop waiting for: a parked chat
//! resolves `user_waiting`, so a `task` call that waits on a subagent lets
//! the turn answer the user instead.

use std::sync::{Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use oven_agent::{PendingInbox, PendingInput};
use tokio::sync::watch;

#[derive(Debug)]
pub(crate) struct TurnInbox {
    items: Mutex<Vec<Parked>>,
    /// How many parked chats are waiting, for a tool that steps aside when
    /// the user writes.
    chats: watch::Sender<usize>,
}

/// One input waiting for a turn.
#[derive(Debug)]
struct Parked {
    input: PendingInput,
    /// Whether it should start a turn of its own when nobody is talking to
    /// the agent. Only a notice may; a subagent the user stopped wakes
    /// nobody. It leaves the inbox with the input it belongs to.
    wakes: bool,
}

impl Default for TurnInbox {
    fn default() -> Self {
        Self {
            items: Mutex::default(),
            chats: watch::Sender::new(0),
        }
    }
}

impl TurnInbox {
    pub(crate) fn push_chat(&self, text: String) {
        let mut items = self.items();
        items.push(Parked {
            input: PendingInput::User(text),
            wakes: false,
        });
        self.publish(&items);
    }

    /// Parks a notice. `wake` asks the runtime to start a turn for it when
    /// the agent is idle.
    pub(crate) fn push_notice(&self, text: String, wake: bool) {
        self.items().push(Parked {
            input: PendingInput::Notice { text },
            wakes: wake,
        });
    }

    /// Keeps the waiting notices for the next turn without starting one for
    /// them: a turn the user cancelled, or one that failed, must not be
    /// followed by a turn nobody asked for.
    ///
    /// It quiets every notice parked at the moment it is called, so it
    /// belongs right after the turn it quiets for: a notice that arrives
    /// afterwards finds an idle agent and wakes it.
    pub(crate) fn quiet(&self) {
        for parked in self.items().iter_mut() {
            parked.wakes = false;
        }
    }

    /// Whether a notice is waiting that should wake an idle agent.
    pub(crate) fn wants_wake(&self) -> bool {
        self.items().iter().any(|parked| parked.wakes)
    }

    /// Drops one parked chat equal to `text`, preferring the latest.
    /// `false` when the running turn already took it.
    pub(crate) fn claim_chat(&self, text: &str) -> bool {
        let mut items = self.items();
        let Some(index) = items.iter().rposition(
            |parked| matches!(&parked.input, PendingInput::User(queued) if queued == text),
        ) else {
            return false;
        };
        items.remove(index);
        self.publish(&items);
        true
    }

    fn publish(&self, items: &[Parked]) {
        let chats = items
            .iter()
            .filter(|parked| matches!(parked.input, PendingInput::User(_)))
            .count();
        self.chats.send_replace(chats);
    }

    fn items(&self) -> MutexGuard<'_, Vec<Parked>> {
        self.items.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl PendingInbox for TurnInbox {
    fn take(&self) -> Vec<PendingInput> {
        let mut items = self.items();
        let taken = std::mem::take(&mut *items);
        self.publish(&items);
        taken.into_iter().map(|parked| parked.input).collect()
    }

    async fn user_waiting(&self) {
        let mut chats = self.chats.subscribe();
        // A closed channel is nobody writing, not somebody who wrote: a
        // caller that took it for a chat would stop waiting on a subagent
        // for no one.
        if chats.wait_for(|count| *count > 0).await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTICE: &str = "explore#1: done · 3 steps, 6 tool calls · 1040 tokens · 32.4s";

    const CHAT: &str = "hello";

    #[test]
    fn a_take_empties_the_inbox() {
        let inbox = TurnInbox::default();
        inbox.push_chat(CHAT.into());
        inbox.push_notice(NOTICE.into(), true);
        assert_eq!(inbox.take().len(), 2);
        assert!(inbox.take().is_empty(), "a second take has nothing left");
        assert!(!inbox.wants_wake(), "a taken notice wakes nobody");
    }

    #[test]
    fn only_a_waking_notice_wants_a_wake() {
        let inbox = TurnInbox::default();
        inbox.push_notice(NOTICE.into(), false);
        assert!(!inbox.wants_wake());
        inbox.push_notice(NOTICE.into(), true);
        assert!(inbox.wants_wake());
    }

    #[test]
    fn a_quieted_notice_stays_for_the_next_turn() {
        let inbox = TurnInbox::default();
        inbox.push_notice(NOTICE.into(), true);
        inbox.quiet();
        assert!(!inbox.wants_wake());
        assert_eq!(inbox.take().len(), 1);
    }

    #[tokio::test]
    async fn a_parked_chat_tells_whoever_waits_for_the_user() {
        let inbox = TurnInbox::default();
        let mut waiting = Box::pin(inbox.user_waiting());
        assert!(
            futures::poll!(waiting.as_mut()).is_pending(),
            "nobody has written yet"
        );
        inbox.push_notice(NOTICE.into(), true);
        assert!(
            futures::poll!(waiting.as_mut()).is_pending(),
            "a notice is not the user"
        );
        inbox.push_chat(CHAT.into());
        waiting.await;
    }

    #[tokio::test]
    async fn a_chat_taken_back_no_longer_waits() {
        let inbox = TurnInbox::default();
        inbox.push_chat(CHAT.into());
        assert!(inbox.claim_chat(CHAT));
        let mut waiting = Box::pin(inbox.user_waiting());
        assert!(futures::poll!(waiting.as_mut()).is_pending());

        inbox.push_chat(CHAT.into());
        inbox.take();
        let mut waiting = Box::pin(inbox.user_waiting());
        assert!(
            futures::poll!(waiting.as_mut()).is_pending(),
            "the turn took it"
        );
    }

    #[test]
    fn claiming_a_chat_prefers_the_latest_one() {
        let inbox = TurnInbox::default();
        inbox.push_chat(CHAT.into());
        inbox.push_notice(NOTICE.into(), true);
        inbox.push_chat(CHAT.into());

        assert!(inbox.claim_chat(CHAT));
        let left = inbox.take();
        assert_eq!(left.len(), 2, "the older chat and the notice stay");
        assert!(matches!(&left[0], PendingInput::User(text) if text == CHAT));
        assert!(
            matches!(&left[1], PendingInput::Notice { text } if text == NOTICE),
            "a notice is never claimed as a chat"
        );
    }

    #[test]
    fn a_notice_keeps_its_shape() {
        let inbox = TurnInbox::default();
        inbox.push_notice(NOTICE.into(), true);
        let taken = inbox.take();
        assert!(
            matches!(taken.as_slice(), [PendingInput::Notice { text }] if text == NOTICE),
            "{taken:?}"
        );
    }
}

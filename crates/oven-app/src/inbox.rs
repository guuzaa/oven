use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::mpsc;

use crate::app::AppError;
use crate::command::Input;

/// The queue of inputs waiting for the conversation driver, which handles
/// them one at a time. It counts the prompts nobody has taken yet, so a
/// frontend can say how many were dropped when the app shut down.
pub(crate) fn channel() -> (InboxSender, InboxReceiver) {
    let (tx, rx) = mpsc::unbounded_channel();
    let queued = Arc::new(AtomicUsize::new(0));
    (
        InboxSender {
            tx,
            queued: Arc::clone(&queued),
        },
        InboxReceiver { rx, queued },
    )
}

pub(crate) struct InboxSender {
    tx: mpsc::UnboundedSender<Input>,
    queued: Arc<AtomicUsize>,
}

impl InboxSender {
    pub(crate) fn send(&self, input: Input) -> Result<(), AppError> {
        let counted = input.is_prompt();
        if counted {
            self.queued.fetch_add(1, Ordering::Relaxed);
        }
        self.tx.send(input).map_err(|_| {
            if counted {
                self.queued.fetch_sub(1, Ordering::Relaxed);
            }
            AppError::ChannelClosed
        })
    }

    pub(crate) fn unsent(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }
}

pub(crate) struct InboxReceiver {
    rx: mpsc::UnboundedReceiver<Input>,
    queued: Arc<AtomicUsize>,
}

impl InboxReceiver {
    pub(crate) async fn recv(&mut self) -> Option<Input> {
        let input = self.rx.recv().await?;
        if input.is_prompt() {
            self.queued.fetch_sub(1, Ordering::Relaxed);
        }
        Some(input)
    }

    pub(crate) fn unsent(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(text: &str) -> Input {
        Input::Chat(text.into())
    }

    #[tokio::test]
    async fn inputs_arrive_in_the_order_they_were_sent() {
        let (tx, mut rx) = channel();
        tx.send(chat("one")).unwrap();
        tx.send(Input::Rewind).unwrap();
        tx.send(chat("two")).unwrap();

        assert_eq!(rx.recv().await, Some(chat("one")));
        assert_eq!(rx.recv().await, Some(Input::Rewind));
        assert_eq!(rx.recv().await, Some(chat("two")));
    }

    #[tokio::test]
    async fn only_prompts_nobody_took_count_as_unsent() {
        let (tx, mut rx) = channel();
        tx.send(chat("taken")).unwrap();
        tx.send(chat("dropped")).unwrap();
        tx.send(Input::Rewind).unwrap();
        assert_eq!(tx.unsent(), 2);

        rx.recv().await.unwrap();

        assert_eq!(tx.unsent(), 1);
        assert_eq!(rx.unsent(), 1);
    }

    #[test]
    fn sending_to_a_closed_inbox_is_not_counted() {
        let (tx, rx) = channel();
        drop(rx);

        assert!(matches!(
            tx.send(chat("lost")),
            Err(AppError::ChannelClosed)
        ));
        assert_eq!(tx.unsent(), 0);
    }
}

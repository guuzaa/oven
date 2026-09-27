//! A question a tool puts to the user, and the channel that carries it to
//! whichever frontend drives the turn. The `answer` tool owns the wire
//! arguments and their limits; this module holds only the shared shape.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

/// A question the model wants the user to answer before it continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub question: String,
    pub options: Vec<QuestionOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QuestionRequestId(pub u64);

impl QuestionRequestId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerResponse {
    Answered {
        answer: String,
    },
    /// The user dismissed the question: the model learns nothing from it.
    Declined,
}

/// One outstanding question awaiting a frontend reply.
pub struct QuestionRequest {
    pub request_id: QuestionRequestId,
    pub question: Question,
    pub responder: oneshot::Sender<AnswerResponse>,
}

pub type QuestionSender = mpsc::UnboundedSender<QuestionRequest>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_unique() {
        assert_ne!(QuestionRequestId::next(), QuestionRequestId::next());
    }
}

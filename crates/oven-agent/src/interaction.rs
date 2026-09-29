//! Everything a running turn asks of the user, and the one channel that
//! carries it. A tool approval, the loop-limit prompt and a tool's question
//! are the same exchange — a request out, exactly one reply back — so they
//! share one id type, one channel and one response enum.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

use crate::identity::ToolCallId;
use crate::tools::ToolView;

pub const NO_USER_TO_ANSWER: &str = "no user is available to answer the question";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UserRequestId(pub u64);

impl UserRequestId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopLimitDecision {
    Continue,
    Exit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerResponse {
    Answered {
        answer: String,
    },
    /// The user dismissed the question: the model learns nothing from it.
    Declined,
}

/// What a turn asks of the user. The reply channel rides along with the
/// request, so the reply needs no id lookup on its way back.
pub enum UserRequest {
    ApproveTool {
        call_id: ToolCallId,
        name: String,
        view: ToolView,
        responder: oneshot::Sender<ApprovalDecision>,
    },
    LoopLimit {
        max_iters: usize,
        responder: oneshot::Sender<LoopLimitDecision>,
    },
    Question {
        question: Question,
        responder: oneshot::Sender<AnswerResponse>,
    },
}

/// One request awaiting a reply.
pub struct PendingRequest {
    pub request_id: UserRequestId,
    pub request: UserRequest,
}

pub type UserRequestSender = mpsc::UnboundedSender<PendingRequest>;

/// What the user answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserResponse {
    Approval(ApprovalDecision),
    LoopLimit(LoopLimitDecision),
    Answer(AnswerResponse),
}

impl PendingRequest {
    /// Replies to the waiting turn. A response of the wrong kind is handed
    /// back with the request, so the oneshot stays open for the reply that
    /// actually answers it.
    pub fn respond(self, response: UserResponse) -> Result<(), Self> {
        match (self.request, response) {
            (UserRequest::ApproveTool { responder, .. }, UserResponse::Approval(decision)) => {
                let _ = responder.send(decision);
                Ok(())
            }
            (UserRequest::LoopLimit { responder, .. }, UserResponse::LoopLimit(decision)) => {
                let _ = responder.send(decision);
                Ok(())
            }
            (UserRequest::Question { responder, .. }, UserResponse::Answer(answer)) => {
                let _ = responder.send(answer);
                Ok(())
            }
            (request, _) => Err(Self {
                request_id: self.request_id,
                request,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_unique() {
        assert_ne!(UserRequestId::next(), UserRequestId::next());
    }

    #[tokio::test]
    async fn a_response_of_the_wrong_kind_leaves_the_request_open() {
        let (responder, reply) = oneshot::channel();
        let request = PendingRequest {
            request_id: UserRequestId::next(),
            request: UserRequest::LoopLimit {
                max_iters: 1,
                responder,
            },
        };
        let request = request
            .respond(UserResponse::Approval(ApprovalDecision::Approved))
            .expect_err("a loop limit is not answered by an approval");
        assert!(
            request
                .respond(UserResponse::LoopLimit(LoopLimitDecision::Exit))
                .is_ok()
        );
        assert_eq!(reply.await.unwrap(), LoopLimitDecision::Exit);
    }

    #[tokio::test]
    async fn a_response_reaches_the_responder_it_answers() {
        let (responder, reply) = oneshot::channel();
        let request = PendingRequest {
            request_id: UserRequestId::next(),
            request: UserRequest::Question {
                question: Question {
                    question: "which database?".into(),
                    options: Vec::new(),
                },
                responder,
            },
        };
        assert!(
            request
                .respond(UserResponse::Answer(AnswerResponse::Declined))
                .is_ok()
        );
        assert_eq!(reply.await.unwrap(), AnswerResponse::Declined);
    }
}

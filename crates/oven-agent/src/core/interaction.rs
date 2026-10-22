//! What a running turn asks of the user, and the one sink that receives it.
//!
//! The exchange itself — the request, the reply, the id that ties them
//! together — belongs to `oven-proto`; what stays here is the machinery that
//! carries the reply back to the turn that is waiting for it.

use std::fmt::Debug;

use tokio::sync::{mpsc, oneshot};

use oven_proto::{
    AnswerResponse, ApprovalDecision, LoopLimitDecision, Question, ToolCallId, TurnId,
    UserRequestId, UserResponse,
};

pub const NO_USER_TO_ANSWER: &str = "no user is available to answer the question";

/// What a turn asks of the user. The reply channel rides along with the
/// request, so the reply needs no id lookup on its way back.
pub enum UserRequest {
    ApproveTool {
        call_id: ToolCallId,
        name: String,
        view: oven_proto::ToolView,
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

/// Where a turn puts what it asks of the user.
pub trait RequestSink: Debug + Send + Sync {
    /// Takes `request` on behalf of `turn_id`. `false` means nobody took it,
    /// so no reply will ever come.
    fn submit(&self, turn_id: TurnId, request: PendingRequest) -> bool;
}

impl RequestSink for mpsc::UnboundedSender<PendingRequest> {
    fn submit(&self, _turn_id: TurnId, request: PendingRequest) -> bool {
        self.send(request).is_ok()
    }
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

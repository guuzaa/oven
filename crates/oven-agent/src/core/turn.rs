#[cfg(test)]
use oven_llm::ModelId;
use std::sync::Arc;

use oven_llm::{Message, Usage};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::core::error::AgentError;
use crate::core::event::{CallOutcome, StepStop};
use crate::core::identity::ToolCallId;
use crate::core::identity::TurnId;
use crate::core::interaction::{
    AnswerResponse, ApprovalDecision, LoopLimitDecision, NO_USER_TO_ANSWER, PendingRequest,
    Question, RequestSink, UserRequest, UserRequestId,
};
use crate::core::mode::AgentMode;
use crate::core::selection::{ModelSelection, Selection};
use crate::core::view::ToolView;

pub const DEFAULT_MAX_ITERS: usize = 200;

/// How much one run may spend before it stops on its own.
///
/// This belongs to the run rather than to `Agent`: the conversation driver
/// outlives any single run, while a subagent or a graph node is entitled to
/// its own budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunPolicy {
    pub max_iters: usize,
}

impl Default for RunPolicy {
    fn default() -> Self {
        Self {
            max_iters: DEFAULT_MAX_ITERS,
        }
    }
}

impl RunPolicy {
    pub fn with_max_iters(mut self, max_iters: usize) -> Self {
        self.max_iters = max_iters;
        self
    }
}

/// What a running turn shares with whoever started it. `selection` is the
/// agent's own, so a mode or model change made while the turn holds
/// `&mut Agent` is picked up at its next step.
#[derive(Debug, Clone)]
pub struct TurnContext {
    pub turn_id: TurnId,
    pub cancellation: CancellationToken,
    selection: Selection,
    policy: RunPolicy,
    /// Absent when nobody can answer: a subagent, or a headless run.
    requests: Option<Arc<dyn RequestSink>>,
}

impl TurnContext {
    pub fn new(turn_id: TurnId, cancellation: CancellationToken, selection: Selection) -> Self {
        Self {
            turn_id,
            cancellation,
            selection,
            policy: RunPolicy::default(),
            requests: None,
        }
    }

    pub fn with_policy(mut self, policy: RunPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn policy(&self) -> RunPolicy {
        self.policy
    }

    pub fn with_requests(mut self, requests: Arc<dyn RequestSink>) -> Self {
        self.requests = Some(requests);
        self
    }

    /// Whether anyone can answer what this turn asks of the user.
    pub fn has_user(&self) -> bool {
        self.requests.is_some()
    }

    /// Puts a request to the user, or `None` when nobody took it.
    fn put<T>(
        &self,
        requests: &Arc<dyn RequestSink>,
        build: impl FnOnce(oneshot::Sender<T>) -> UserRequest,
    ) -> Option<oneshot::Receiver<T>> {
        let (responder, reply) = oneshot::channel();
        let request = PendingRequest {
            request_id: UserRequestId::next(),
            request: build(responder),
        };
        requests.submit(self.turn_id, request).then_some(reply)
    }

    /// Puts a tool call to the user. A turn nobody can answer for refuses the
    /// call rather than hanging on a reply that will never come.
    pub async fn approve(
        &self,
        call_id: ToolCallId,
        name: String,
        view: ToolView,
    ) -> Option<ApprovalDecision> {
        let Some(requests) = &self.requests else {
            return Some(ApprovalDecision::Rejected);
        };
        let reply = self.put(requests, |responder| UserRequest::ApproveTool {
            call_id,
            name,
            view,
            responder,
        })?;
        self.wait(reply).await
    }

    /// Puts `question` to the user and waits for the reply, giving up as
    /// cancelled when the turn is cancelled or the frontend goes away.
    pub async fn ask(&self, question: Question) -> Result<AnswerResponse, AgentError> {
        let Some(requests) = &self.requests else {
            return Err(AgentError::from(NO_USER_TO_ANSWER));
        };
        let reply = self
            .put(requests, |responder| UserRequest::Question {
                question,
                responder,
            })
            .ok_or_else(|| AgentError::from(NO_USER_TO_ANSWER))?;
        self.wait(reply).await.ok_or_else(AgentError::cancelled)
    }

    /// Asks whether a turn that hit its step budget may keep going. Without
    /// anyone to ask, the budget stands.
    pub async fn request_loop_continue(&self, max_iters: usize) -> Option<LoopLimitDecision> {
        let Some(requests) = &self.requests else {
            return Some(LoopLimitDecision::Exit);
        };
        let reply = self.put(requests, |responder| UserRequest::LoopLimit {
            max_iters,
            responder,
        })?;
        self.wait(reply).await
    }

    /// Waits for one reply, giving up as `None` when the turn is cancelled or
    /// the frontend goes away before answering.
    async fn wait<T>(&self, reply: oneshot::Receiver<T>) -> Option<T> {
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => None,
            response = reply => response.ok(),
        }
    }

    pub fn mode(&self) -> AgentMode {
        self.selection.mode()
    }

    pub fn model(&self) -> ModelSelection {
        self.selection.model()
    }
}

#[derive(Debug, Clone)]
pub struct TurnOutput {
    pub response: Message,
    pub usage: Usage,
}

/// What one provider round trip did: the assistant reply, the tool calls it
/// asked for, and what the provider charged for it.
///
/// This is what a loop strategy reads between steps. Tool output is not
/// repeated here — it is the tool-result message the step appended to the
/// history — so a step carrying a huge `file_read` result stays cheap.
#[derive(Debug, Clone)]
pub struct Step {
    pub text: String,
    pub calls: Vec<StepCall>,
    pub usage: Option<Usage>,
}

impl Step {
    /// A step that asked for no tool calls ends the loop.
    pub fn is_final(&self) -> bool {
        self.calls.is_empty()
    }

    pub fn stop(&self) -> StepStop {
        if self.is_final() {
            StepStop::FinalAnswer
        } else {
            StepStop::ToolUse
        }
    }
}

/// One tool call a step dispatched, as the loop sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepCall {
    pub call_id: ToolCallId,
    pub name: String,
    pub outcome: CallOutcome,
}

#[cfg(test)]
impl TurnContext {
    /// A bare context for tests that exercise a tool without driving a turn.
    pub(crate) fn for_test() -> Self {
        Self::for_test_in(AgentMode::Agent)
    }

    pub(crate) fn for_test_in(mode: AgentMode) -> Self {
        Self::new(
            TurnId::next(),
            CancellationToken::new(),
            Selection::new(mode, ModelId::new("default"), None),
        )
    }
}

impl TurnOutput {
    pub fn text(&self) -> String {
        self.response
            .content
            .iter()
            .filter_map(|b| match b {
                oven_llm::ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

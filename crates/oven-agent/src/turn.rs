use std::sync::{Arc, Mutex, PoisonError};

use oven_llm::{Message, ModelId, ReasoningEffort, Usage};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::approval::{
    ApprovalDecision, ApprovalSender, LoopLimitDecision, LoopLimitPrompt, LoopLimitRequestId,
    LoopLimitSender, ToolApproval,
};
use crate::error::AgentError;
use crate::event::{CallOutcome, StepStop};
use crate::identity::ToolCallId;
use crate::identity::TurnId;
use crate::mode::AgentMode;
use crate::question::{
    AnswerResponse, NO_USER_TO_ANSWER, Question, QuestionRequest, QuestionRequestId, QuestionSender,
};
use crate::tools::ToolView;

pub const DEFAULT_MAX_ITERS: usize = 200;

pub type ModelSelection = (ModelId, Option<ReasoningEffort>);

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

/// Shared, per-turn state that a running turn re-reads at each step.
///
/// `mode` and `model` live behind a lock (rather than as `Agent` fields)
/// specifically so control commands can update them while a turn holds
/// `&mut Agent` exclusively: the turn picks up the change at its next step
/// instead of waiting for the whole turn to finish.
#[derive(Debug, Clone)]
pub struct TurnContext {
    pub turn_id: TurnId,
    pub cancellation: CancellationToken,
    mode: Arc<Mutex<AgentMode>>,
    model: Arc<Mutex<ModelSelection>>,
    policy: RunPolicy,
    approval_sender: Option<ApprovalSender>,
    loop_limit_sender: Option<LoopLimitSender>,
    question_sender: Option<QuestionSender>,
}

impl TurnContext {
    pub fn new(
        turn_id: TurnId,
        cancellation: CancellationToken,
        mode: AgentMode,
        model: ModelId,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Self {
        Self {
            turn_id,
            cancellation,
            mode: Arc::new(Mutex::new(mode)),
            model: Arc::new(Mutex::new((model, reasoning_effort))),
            policy: RunPolicy::default(),
            approval_sender: None,
            loop_limit_sender: None,
            question_sender: None,
        }
    }

    pub fn with_policy(mut self, policy: RunPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn policy(&self) -> RunPolicy {
        self.policy
    }

    pub fn with_approval_sender(mut self, approval_sender: ApprovalSender) -> Self {
        self.approval_sender = Some(approval_sender);
        self
    }

    pub fn with_loop_limit_sender(mut self, loop_limit_sender: LoopLimitSender) -> Self {
        self.loop_limit_sender = Some(loop_limit_sender);
        self
    }

    pub fn with_question_sender(mut self, question_sender: QuestionSender) -> Self {
        self.question_sender = Some(question_sender);
        self
    }

    /// The channel interactive tools use to put a question to the user.
    pub fn question_sender(&self) -> Option<&QuestionSender> {
        self.question_sender.as_ref()
    }

    pub fn has_loop_limit_sender(&self) -> bool {
        self.loop_limit_sender.is_some()
    }

    pub async fn request_approval(
        &self,
        request_id: crate::approval::ApprovalRequestId,
        call_id: ToolCallId,
        name: String,
        view: ToolView,
    ) -> Option<ApprovalDecision> {
        let Some(sender) = self.approval_sender.as_ref() else {
            return Some(ApprovalDecision::Rejected);
        };
        let (responder, response) = oneshot::channel();
        sender
            .send(ToolApproval {
                request_id,
                call_id,
                name,
                view,
                responder,
            })
            .ok()?;
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => None,
            decision = response => decision.ok(),
        }
    }

    /// Puts `question` to the user and waits for the reply, giving up as
    /// cancelled when the turn is cancelled or the frontend goes away.
    pub async fn ask(&self, question: Question) -> Result<AnswerResponse, AgentError> {
        let asker = self
            .question_sender
            .as_ref()
            .ok_or_else(|| AgentError::from(NO_USER_TO_ANSWER))?;
        let (responder, response) = oneshot::channel();
        asker
            .send(QuestionRequest {
                request_id: QuestionRequestId::next(),
                question,
                responder,
            })
            .map_err(|_| AgentError::from(NO_USER_TO_ANSWER))?;
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => Err(AgentError::cancelled()),
            reply = response => reply.map_err(|_| AgentError::cancelled()),
        }
    }

    pub async fn request_loop_continue(
        &self,
        request_id: LoopLimitRequestId,
        max_iters: usize,
    ) -> Option<LoopLimitDecision> {
        let Some(sender) = self.loop_limit_sender.as_ref() else {
            return Some(LoopLimitDecision::Exit);
        };
        let (responder, response) = oneshot::channel();
        sender
            .send(LoopLimitPrompt {
                request_id,
                max_iters,
                responder,
            })
            .ok()?;
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => None,
            decision = response => decision.ok(),
        }
    }

    pub fn set_mode(&self, mode: AgentMode) {
        *self.mode.lock().unwrap_or_else(PoisonError::into_inner) = mode;
    }

    pub fn mode(&self) -> AgentMode {
        *self.mode.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_model(&self, model: ModelId, reasoning_effort: Option<ReasoningEffort>) {
        *self.model.lock().unwrap_or_else(PoisonError::into_inner) = (model, reasoning_effort);
    }

    pub fn model(&self) -> ModelSelection {
        self.model
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
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
        Self::new(
            TurnId::next(),
            CancellationToken::new(),
            AgentMode::Agent,
            ModelId::new("default"),
            None,
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

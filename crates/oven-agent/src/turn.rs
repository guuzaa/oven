use std::sync::{Arc, Mutex};

use oven_llm::{Message, ModelId, ReasoningEffort, Usage};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::approval::{ApprovalDecision, ApprovalSender, ToolApproval};
use crate::identity::ToolCallId;
use crate::identity::TurnId;
use crate::mode::AgentMode;
use crate::tools::ToolView;

type ModelSelection = (ModelId, Option<ReasoningEffort>);

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
    approval_sender: Option<ApprovalSender>,
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
            approval_sender: None,
        }
    }

    pub fn with_approval_sender(mut self, approval_sender: ApprovalSender) -> Self {
        self.approval_sender = Some(approval_sender);
        self
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
            _ = self.cancellation.cancelled() => None,
            decision = response => decision.ok(),
        }
    }

    pub fn set_mode(&self, mode: AgentMode) {
        *self.mode.lock().unwrap_or_else(|e| e.into_inner()) = mode;
    }

    pub fn mode(&self) -> AgentMode {
        *self.mode.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_model(&self, model: ModelId, reasoning_effort: Option<ReasoningEffort>) {
        *self.model.lock().unwrap_or_else(|e| e.into_inner()) = (model, reasoning_effort);
    }

    pub fn model(&self) -> ModelSelection {
        self.model.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[derive(Debug, Clone)]
pub struct TurnOutput {
    pub response: Message,
    pub usage: Usage,
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

use std::sync::{Arc, Mutex};

use oven_llm::{Message, ModelId, ReasoningEffort, Usage};
use tokio_util::sync::CancellationToken;

use crate::identity::TurnId;
use crate::mode::AgentMode;

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

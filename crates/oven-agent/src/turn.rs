use std::sync::{Arc, Mutex};

use oven_llm::Message;
use oven_llm::Usage;
use tokio_util::sync::CancellationToken;

use crate::identity::TurnId;
use crate::mode::AgentMode;

#[derive(Debug, Clone)]
pub struct TurnContext {
    pub turn_id: TurnId,
    pub cancellation: CancellationToken,
    mode: Arc<Mutex<AgentMode>>,
}

impl TurnContext {
    pub fn new(turn_id: TurnId, cancellation: CancellationToken, mode: AgentMode) -> Self {
        Self {
            turn_id,
            cancellation,
            mode: Arc::new(Mutex::new(mode)),
        }
    }

    pub fn set_mode(&self, mode: AgentMode) {
        *self.mode.lock().unwrap_or_else(|e| e.into_inner()) = mode;
    }

    pub fn mode(&self) -> AgentMode {
        *self.mode.lock().unwrap_or_else(|e| e.into_inner())
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

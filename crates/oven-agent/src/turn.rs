use oven_llm::Message;
use oven_llm::Usage;
use tokio_util::sync::CancellationToken;

use crate::identity::TurnId;

#[derive(Debug, Clone)]
pub struct TurnContext {
    pub turn_id: TurnId,
    pub cancellation: CancellationToken,
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

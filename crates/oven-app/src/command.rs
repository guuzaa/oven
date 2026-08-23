use oven_agent::{AgentMode, TurnId};
use oven_llm::ReasoningEffort;

use crate::config::ProviderConfig;

#[derive(Debug, Clone)]
pub enum AppCommand {
    StartTurn {
        input: String,
    },
    Cancel {
        turn_id: TurnId,
    },
    Rewind,
    ClearSession,
    SetMode {
        mode: AgentMode,
    },
    SetModel {
        model: String,
        reasoning_effort: Option<ReasoningEffort>,
    },
    SetProvider {
        provider: ProviderConfig,
    },
    Shutdown,
}

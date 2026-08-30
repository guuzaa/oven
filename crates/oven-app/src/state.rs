use oven_agent::{AgentMode, TodoList, TurnId};
use oven_llm::{Message, ReasoningEffort, Usage};

use crate::config::ProviderConfig;

#[derive(Debug, Clone)]
pub struct AppState {
    pub phase: AppPhase,
    pub mode: AgentMode,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub provider: ProviderConfig,
    pub configured_providers: Vec<String>,
    pub history: Vec<Message>,
    pub todos: TodoList,
    pub last_turn_usage: Usage,
    pub session: SessionState,
    pub models: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppPhase {
    Idle,
    Running { turn_id: TurnId },
    Cancelling { turn_id: TurnId },
    ShuttingDown,
}

impl AppPhase {
    pub fn turn_id(self) -> Option<TurnId> {
        match self {
            Self::Running { turn_id } | Self::Cancelling { turn_id } => Some(turn_id),
            Self::Idle | Self::ShuttingDown => None,
        }
    }

    pub fn is_idle(self) -> bool {
        matches!(self, Self::Idle)
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Running { .. } | Self::Cancelling { .. })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionState {
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateEvent {
    pub revision: u64,
    pub change: StateChange,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StateChange {
    ModelChanged {
        model: String,
        reasoning_effort: Option<ReasoningEffort>,
    },
    ModeChanged {
        mode: AgentMode,
    },
    TodosChanged {
        todos: TodoList,
    },
    HistoryChanged {
        revision: u64,
    },
    SessionChanged {
        session_id: Option<String>,
    },
    UsageChanged {
        usage: Usage,
    },
    ProviderChanged {
        provider: ProviderConfig,
        configured_providers: Vec<String>,
    },
    ModelsChanged {
        models: Vec<(String, String)>,
    },
}

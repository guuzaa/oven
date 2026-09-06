use oven_agent::{Agent, AgentMode, TodoList, TurnId};
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
    /// Unix-ms timestamps parallel to `history`, taken from `Record`.
    pub history_timestamps: Vec<u64>,
    /// Thinking duration in ms parallel to `history`; `None` if that message
    /// had no timed thinking.
    pub history_thinking_ms: Vec<Option<u64>>,
    pub todos: TodoList,
    pub last_turn_usage: Usage,
    /// Prompt-side tokens of the last response in the current turn; approximates
    /// the current context size. Zero until a turn completes.
    pub context_tokens: u32,
    /// Context window of the active model, when known.
    pub context_window: Option<u32>,
    pub session: SessionState,
    pub models: Vec<(String, String)>,
}

impl AppState {
    pub(crate) fn from_agent(
        agent: &Agent,
        provider: ProviderConfig,
        configured_providers: Vec<String>,
        session: SessionState,
    ) -> Self {
        Self {
            phase: AppPhase::Idle,
            mode: agent.mode(),
            model: agent.model().to_string(),
            reasoning_effort: agent.reasoning_effort(),
            provider,
            configured_providers,
            history: agent.history().cloned().collect(),
            history_timestamps: agent.history_timed().map(|(_, ts, _)| ts).collect(),
            history_thinking_ms: agent.history_timed().map(|(_, _, th)| th).collect(),
            todos: agent.todos().clone(),
            last_turn_usage: agent.last_turn_usage(),
            context_tokens: context_tokens(agent),
            context_window: context_window(agent),
            session,
            models: Vec::new(),
        }
    }
}

/// Prompt-side tokens (input + cache reads) of the last response in the
/// current turn.
pub(crate) fn context_tokens(agent: &Agent) -> u32 {
    let usage = agent.last_turn_usage();
    usage.input_tokens.saturating_add(usage.cache_read_tokens)
}

/// Context window of the agent's active model, when the router knows it.
pub(crate) fn context_window(agent: &Agent) -> Option<u32> {
    use oven_llm::Provider;
    agent
        .router()
        .resolve_model(agent.model())
        .map(|info| info.context_window)
        .filter(|window| *window > 0)
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
    ContextChanged {
        tokens: u32,
        window: Option<u32>,
    },
    ProviderChanged {
        provider: ProviderConfig,
        configured_providers: Vec<String>,
    },
    ModelsChanged {
        models: Vec<(String, String)>,
    },
}

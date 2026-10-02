use std::sync::Arc;

use oven_agent::{Agent, AgentId, AgentMode, NodeInfo, TodoList, TurnId};
use oven_llm::{Message, ModelId, Provider, ReasoningEffort, Router, Usage};

use crate::core::config::ProviderConfig;

#[derive(Debug, Clone)]
pub struct AppState {
    pub phase: AppPhase,
    /// The conversation driver. Every other agent a view sees is a subagent,
    /// which is how an event is routed to the transcript it belongs to.
    pub agent_id: AgentId,
    /// Subagents in spawn order, mirrored from the registry. Shared, so
    /// publishing state and telling the frontend it moved cost a refcount
    /// rather than a copy of every subagent.
    pub subagents: Arc<Vec<NodeInfo>>,
    pub mode: AgentMode,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub provider: ProviderConfig,
    pub configured_providers: Vec<String>,
    /// Shared handles to the agent's messages: publishing state (and every
    /// `watch` clone it triggers) then costs refcounts, not the conversation.
    pub history: Vec<Arc<Message>>,
    /// Unix-ms timestamps parallel to `history`, taken from `Record`.
    pub history_timestamps: Vec<u64>,
    /// Thinking duration in ms parallel to `history`; `None` if that message
    /// had no timed thinking.
    pub history_thinking_ms: Vec<Option<u64>>,
    pub todos: TodoList,
    pub last_turn_usage: Usage,
    /// Tokens of the last response in the current turn that the context holds:
    /// its input plus the output that joins the history. Zero until a turn
    /// completes.
    pub context_tokens: u32,
    /// Context window of the active model, when known.
    pub context_window: Option<u32>,
    pub session: SessionState,
    pub models: Vec<(String, String)>,
}

impl AppState {
    /// The conversation with its record timestamps and thinking durations,
    /// sharing the agent's messages so a transcript re-seed does not copy
    /// every message.
    pub fn history_timed_shared(&self) -> Vec<(Arc<Message>, u64, Option<u64>)> {
        self.history
            .iter()
            .cloned()
            .zip(self.history_timestamps.iter().copied())
            .zip(self.history_thinking_ms.iter().copied())
            .map(|((message, timestamp), thinking_ms)| (message, timestamp, thinking_ms))
            .collect()
    }

    pub(crate) fn from_agent(
        agent: &Agent,
        provider: ProviderConfig,
        configured_providers: Vec<String>,
        session: SessionState,
    ) -> Self {
        Self {
            phase: AppPhase::Idle,
            agent_id: agent.id(),
            subagents: Arc::new(Vec::new()),
            mode: agent.mode(),
            model: agent.model().to_string(),
            reasoning_effort: agent.reasoning_effort(),
            provider,
            configured_providers,
            history: agent.shared_history(),
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

/// Tokens the last response in the current turn leaves in the context: the
/// input it sent (cache reads included) plus the output it added to the
/// history.
pub(crate) fn context_tokens(agent: &Agent) -> u32 {
    let usage = agent.last_turn_usage();
    usage.input_tokens.saturating_add(usage.output_tokens)
}

/// Context window of `model`, when the router knows it.
pub(crate) fn context_window_of(router: &Router, model: &str) -> Option<u32> {
    router
        .resolve_model(&ModelId::from(model))
        .map(|info| info.context_window)
        .filter(|window| *window > 0)
}

/// Context window of the agent's active model, when the router knows it.
pub(crate) fn context_window(agent: &Agent) -> Option<u32> {
    context_window_of(&agent.router(), agent.model().as_str())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppPhase {
    Idle,
    Running {
        turn_id: TurnId,
    },
    /// The turn is blocked on the user: a tool approval, the loop limit or a
    /// question. What it is waiting for reaches a frontend as the agent event
    /// that announced it.
    Awaiting {
        turn_id: TurnId,
    },
    Cancelling {
        turn_id: TurnId,
    },
    /// The history is being summarized: the driver is busy, but there is no
    /// turn to cancel.
    Compacting,
    ShuttingDown,
}

impl AppPhase {
    pub fn turn_id(&self) -> Option<TurnId> {
        match self {
            Self::Running { turn_id }
            | Self::Awaiting { turn_id }
            | Self::Cancelling { turn_id } => Some(*turn_id),
            Self::Idle | Self::Compacting | Self::ShuttingDown => None,
        }
    }

    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    /// Whether the driver is occupied, so a frontend shows itself busy.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Running { .. }
                | Self::Awaiting { .. }
                | Self::Cancelling { .. }
                | Self::Compacting
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionState {
    pub id: Option<String>,
}

/// Why the conversation history was replaced, so a view can rebuild itself
/// and tell a rewind from a `/clear`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryChangeReason {
    /// Esc rewind truncated the last turn.
    Rewound,
    /// `/clear` dropped the conversation.
    Cleared,
    /// `/compact` or auto-compaction replaced it with a summary.
    Compacted,
    /// History was replaced by something outside a known command.
    External,
}

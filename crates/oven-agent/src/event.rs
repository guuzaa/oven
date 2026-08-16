use oven_llm::{ReasoningEffort, Usage};

use crate::todo::TodoItem;

/// Stable id for an agent instance (main or sub-agent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AgentId(pub u64);

/// Events emitted during one agent turn. No app_id here — the App layer
/// envelopes these when forwarding to the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    ThinkingDelta {
        agent_id: AgentId,
        text: String,
    },
    TextDelta {
        agent_id: AgentId,
        text: String,
    },
    ToolStart {
        agent_id: AgentId,
        call_id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolEnd {
        agent_id: AgentId,
        call_id: String,
        ok: bool,
        output: String,
    },
    Done {
        agent_id: AgentId,
        text: String,
        usage: Usage,
    },
    Cancelled {
        agent_id: AgentId,
    },
    HistoryCleared {
        agent_id: AgentId,
    },
    ModelChanged {
        agent_id: AgentId,
        model: String,
        reasoning_effort: Option<ReasoningEffort>,
    },
    TodoUpdated {
        agent_id: AgentId,
        items: Vec<TodoItem>,
    },
}

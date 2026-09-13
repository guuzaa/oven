use oven_llm::Usage;

use crate::approval::ApprovalRequestId;
use crate::error::AgentError;
use crate::identity::{AgentId, ToolCallId, TurnId};
use crate::todo::TodoList;
use crate::tools::ToolView;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEventEnvelope {
    pub seq: u64,
    pub agent_id: AgentId,
    pub turn_id: TurnId,
    pub event: AgentEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    Turn(TurnEvent),
    Stream(StreamEvent),
    Tool(ToolEvent),
    TodosChanged { todos: TodoList },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEvent {
    Started,
    Completed { usage: Usage, duration_ms: u64 },
    Cancelled { duration_ms: u64 },
    Failed { error: AgentError, duration_ms: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    TextDelta { text: String },
    ThinkingDelta { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolEvent {
    ApprovalRequested {
        request_id: ApprovalRequestId,
        call_id: ToolCallId,
        name: String,
        view: ToolView,
    },
    Started {
        call_id: ToolCallId,
        name: String,
        view: ToolView,
    },
    OutputDelta {
        call_id: ToolCallId,
        stream: ToolOutputStream,
        text: String,
    },
    Finished {
        call_id: ToolCallId,
        result: ToolResult,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolResult {
    Success {
        output: String,
    },
    Failed {
        error: String,
        output: Option<String>,
    },
    Rejected {
        reason: String,
    },
    Cancelled,
}

impl ToolResult {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    pub fn output(&self) -> &str {
        match self {
            Self::Success { output } => output,
            Self::Failed { output, error } => output.as_deref().unwrap_or(error),
            Self::Rejected { reason } => reason,
            Self::Cancelled => "",
        }
    }
}

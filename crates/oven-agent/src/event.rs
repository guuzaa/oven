use oven_llm::Usage;

use crate::approval::{ApprovalRequestId, LoopLimitRequestId};
use crate::error::AgentError;
use crate::identity::{AgentId, ToolCallId, TurnId};
use crate::question::{Question, QuestionRequestId};
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
    Usage { usage: Usage },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEvent {
    Started,
    /// A provider round trip begins. `index` counts from 1 within the turn,
    /// so a driver can report loop progress without reading the agent.
    StepStarted {
        index: usize,
    },
    StepFinished {
        index: usize,
        stop: StepStop,
    },
    Completed {
        usage: Usage,
        duration_ms: u64,
    },
    Cancelled {
        duration_ms: u64,
    },
    Failed {
        error: AgentError,
        duration_ms: u64,
    },
    LoopLimitReached {
        request_id: LoopLimitRequestId,
        max_iters: usize,
    },
}

/// Why a step ended, which is also what the loop does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStop {
    /// The assistant asked for tools; the loop runs them and continues.
    ToolUse,
    /// The assistant answered; the loop stops.
    FinalAnswer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    /// Closes the thinking window started by the preceding `ThinkingDelta`s.
    /// The agent owns the clock, so the transcript renders this duration
    /// instead of timing the deltas itself. It is emitted the moment the
    /// reasoning phase ends, ahead of the answer text or the tool call that
    /// ended it.
    ThinkingDone {
        duration_ms: u64,
    },
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
    /// A tool is waiting for the user to answer `question`. A question
    /// originates inside the tool call that asked it, so this event is
    /// emitted by the frontend that owns the answer channel rather than by
    /// the agent loop.
    QuestionAsked {
        request_id: QuestionRequestId,
        question: Question,
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

/// How a tool call ended, without its output: the output is the tool-result
/// message the step appended to the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    Success,
    Failed,
    Rejected,
    Cancelled,
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

    pub fn outcome(&self) -> CallOutcome {
        match self {
            Self::Success { .. } => CallOutcome::Success,
            Self::Failed { .. } => CallOutcome::Failed,
            Self::Rejected { .. } => CallOutcome::Rejected,
            Self::Cancelled => CallOutcome::Cancelled,
        }
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

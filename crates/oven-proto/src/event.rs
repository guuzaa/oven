//! What happened while a turn ran.
//!
//! Every event names the agent and the turn it belongs to, so a frontend
//! can route it to the conversation it belongs to and ignore the rest.

use oven_llm::Usage;
use serde::{Deserialize, Serialize};

use crate::{AgentError, AgentId, Question, TodoList, ToolCallId, ToolView, TurnId, UserRequestId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEventEnvelope {
    pub agent_id: AgentId,
    pub turn_id: TurnId,
    pub event: AgentEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum AgentEvent {
    Turn(TurnEvent),
    Stream(StreamEvent),
    Tool(ToolEvent),
    TodosChanged { todos: TodoList },
    Usage { usage: Usage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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
    /// A chat typed while the turn was running, appended after the tool
    /// results that are about to be sent.
    UserAppended {
        text: String,
    },
    /// Something that happened on its own — a subagent finishing, say —
    /// appended the same way. A frontend draws it as its own line rather
    /// than as something the user typed.
    NoticeAppended {
        text: String,
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
        request_id: UserRequestId,
        max_iters: usize,
    },
}

/// Why a step ended, which is also what the loop does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStop {
    /// The assistant asked for tools; the loop runs them and continues.
    ToolUse,
    /// The assistant answered; the loop stops.
    FinalAnswer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolEvent {
    ApprovalRequested {
        request_id: UserRequestId,
        call_id: ToolCallId,
        name: String,
        view: ToolView,
    },
    Started {
        call_id: ToolCallId,
        name: String,
        view: ToolView,
    },
    /// A tool is waiting for the user to answer `question`. The runtime
    /// projects this from the request, as it does for an approval and the
    /// loop limit: the agent loop does not emit user prompts itself.
    QuestionAsked {
        request_id: UserRequestId,
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
        /// The body to show under the call's row once it has landed
        detail: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputStream {
    Stdout,
    Stderr,
}

/// How a tool call ended, without its output: the output is the tool-result
/// message the step appended to the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOutcome {
    Success,
    Failed,
    Rejected,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

    /// The tool was invoked. A refusal never ran, so it is not a call.
    pub fn executed(&self) -> bool {
        matches!(self, Self::Success { .. } | Self::Failed { .. })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_event_survives_the_wire() {
        let event = AgentEvent::Tool(ToolEvent::Started {
            call_id: ToolCallId(7),
            name: "read".into(),
            view: ToolView::named("Read src/lib.rs"),
        });
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            serde_json::from_str::<AgentEvent>(&json).unwrap(),
            event,
            "the wire form must round-trip"
        );
    }

    #[test]
    fn a_turn_event_survives_the_wire() {
        let event = AgentEvent::Turn(TurnEvent::StepFinished {
            index: 2,
            stop: StepStop::ToolUse,
        });
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), event);
    }
}

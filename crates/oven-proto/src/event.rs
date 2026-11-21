//! What happened while a turn ran.
//!
//! Every event names the agent and the turn it belongs to, so a frontend
//! can route it to the conversation it belongs to and ignore the rest, and
//! carries the moment it happened, so a trace reads as a timeline and not
//! only as an order.

use oven_llm::{StopReason, Usage};
use serde::{Deserialize, Serialize};

use crate::{AgentError, AgentId, Question, TodoList, ToolCallId, ToolView, TurnId, UserRequestId};

/// The target every fact about a run is logged under.
///
/// The agent writes the facts it has, the app writes them again as a
/// projection of the event stream, and both land under this target: one
/// filter then keeps the whole run, and nothing about it is lost between
/// the layer that knows and the layer that reads.
pub const AGENT_TARGET: &str = "oven.agent";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEventEnvelope {
    pub agent_id: AgentId,
    pub turn_id: TurnId,
    /// Unix milliseconds when the event was emitted.
    pub at: u64,
    /// Milliseconds since the run started, on a monotonic clock.
    ///
    /// `at` says when the event happened in wall time, which is what orders
    /// it against the rest of the world. This says how far into the run it
    /// happened, and it keeps saying so when the wall clock is corrected
    /// under the run's feet.
    ///
    /// A line written before the clock landed reads as zero: a moment the
    /// trace cannot place, kept rather than dropped.
    #[serde(default)]
    pub elapsed_ms: u64,
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
    /// The turn's first user message: what it was asked to do. A subagent's
    /// task is raised by the runtime, which is the only one that knows it —
    /// the agent loop is handed the prompt, not an event about it.
    Prompt {
        text: String,
    },
    /// A provider round trip begins. `index` counts from 1 within the turn,
    /// so a driver can report loop progress without reading the agent.
    StepStarted {
        index: usize,
    },
    StepFinished {
        index: usize,
        stop: StepStop,
        /// How long the provider took to answer this step, retries
        /// included. The step's own wall time is the gap between this
        /// event's timestamp and the one that started it.
        provider_ms: u64,
    },
    /// The provider answered, but the reply cannot end the turn: it
    /// carries no answer, or it was cut off.
    ///
    /// It says what the provider sent, so a reader can tell a model that
    /// stopped on its own from one that was cut off, and a reader of a
    /// trace can see the facts without the log.
    StepRejected {
        /// Which step of the turn it was.
        index: usize,
        /// Why the reply could not end the turn.
        error: AgentError,
        /// What the provider said it stopped for.
        stop_reason: Option<StopReason>,
        /// How many content blocks the reply carried.
        blocks: usize,
        /// What the provider said it used.
        usage: Option<Usage>,
        /// How long the provider took to answer.
        provider_ms: u64,
    },
    /// The provider had to be asked again: the call waited, and then asked
    /// once more. One event per wait, so the timeline shows where the wait
    /// was rather than only how long it was.
    ProviderRetried {
        /// Which retry this was: 1 is the first time the call was asked
        /// again. A line written before the field was renamed calls it
        /// `attempts`, and reads all the same.
        #[serde(alias = "attempts")]
        retry: u32,
        /// How long the call waited before asking again, in milliseconds.
        waited_ms: u64,
        /// The status the attempt that failed reported, when it had one.
        status: Option<u16>,
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
        /// The tool the call ran, so the event stands on its own.
        name: String,
        result: ToolResult,
        /// The body to show under the call's row once it has landed
        detail: Option<String>,
        /// How long the call ran. Zero when it never ran: a call the gate
        /// refused, or one the user declined.
        duration_ms: u64,
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
            provider_ms: 120,
        });
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), event);
    }

    #[test]
    fn a_rejected_reply_survives_the_wire() {
        let event = AgentEvent::Turn(TurnEvent::StepRejected {
            index: 1,
            error: AgentError::from("the model returned no answer"),
            stop_reason: Some(StopReason::EndTurn),
            blocks: 1,
            usage: Some(Usage {
                input_tokens: 1200,
                output_tokens: 653,
                cache_read_tokens: 0,
                reasoning_tokens: 653,
            }),
            provider_ms: 410,
        });
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), event);
    }

    #[test]
    fn a_raised_prompt_survives_the_wire() {
        let event = AgentEvent::Turn(TurnEvent::Prompt {
            text: "read note.txt".into(),
        });
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), event);
    }
}

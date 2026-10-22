//! The protocol layer: the vocabulary a frontend and the app runtime share.
//!
//! Everything a frontend can observe or request is named here — the events
//! that report what happened, the state that says what the system is now,
//! and the commands that ask for something to be done. The crate is a leaf:
//! it depends on `oven-llm` for messages and tokens and on serde for the
//! wire format, and on nothing else. No runtime, no filesystem, no clock.
//!
//! That is the point. A frontend that speaks this crate — a terminal, a web
//! page, a remote client — can be written without the agent runtime in its
//! dependency tree, and the app runtime can be written without knowing how
//! any of them render what it reports.

mod app_event;
mod command;
mod error;
mod event;
mod id;
mod input;
mod interaction;
mod mode;
mod permission;
mod record;
mod sink;
mod state;
mod subagent;
mod todo;
mod view;

pub use app_event::{
    AppEvent, AppEventKind, AppId, COMPACTION_CANCELLED, CompactionEvent, ShellEvent, SubagentEvent,
};
pub use command::{Command, CommandResult};
pub use error::{AgentError, MAX_ITERS_EXCEEDED};
pub use event::{
    AgentEvent, AgentEventEnvelope, CallOutcome, StepStop, StreamEvent, ToolEvent,
    ToolOutputStream, ToolResult, TurnEvent,
};
pub use id::{AgentId, ToolCallId, TurnId, UserRequestId};
pub use input::Input;
pub use interaction::{
    AnswerResponse, ApprovalDecision, LoopLimitDecision, Question, QuestionOption, UserResponse,
};
pub use mode::{AgentMode, ToolAccess};
pub use permission::ToolPermission;
pub use record::{Record, SessionMeta};
pub use sink::{EventSink, NullSink, VecEventSink};
pub use state::{AppLevels, AppPhase, AppState, HistoryChangeReason, ProviderView, SessionState};
pub use subagent::{NodeInfo, NodeStatus, READ_RESULT_HINT, progress};
pub use todo::{TodoItem, TodoList, TodoStatus};
pub use view::ToolView;

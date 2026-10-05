//! The agent layer: a driver that runs turns against a provider, the tools a
//! turn may call, and the vocabulary both share.
//!
//! Layers, top to bottom; a layer reaches only into the ones below it:
//!
//! 1. `runtime` — the `Agent` driver and the turns it runs
//! 2. `capabilities` — what an agent can call: the `Tool` protocol, the
//!    built-in tools, skills, the protocol for handing work to another agent
//! 3. `core` — the nouns and their pure rules: identity, events, history and
//!    its records, the turn context, the user-request protocol, tool views
//!
//! Providers and messages are `oven-llm`; files, processes and the clock are
//! `oven-host`. Both sit below all three layers.

mod capabilities;
mod core;
mod runtime;

pub use capabilities::skills::{Skill, SkillRegistry};
pub use capabilities::tools::{
    AnswerTool, BUILTIN_TOOLS, BashTool, BuiltinTool, FileEditTool, FileReadTool, FileWriteTool,
    GlobTool, GrepTool, ListModelsTool, SkillReadTool, TaskOutputTool, TaskTool, TodoWriteTool,
    Tool, WebFetchTool, present_tool,
};
pub use core::error::{AgentError, MAX_ITERS_EXCEEDED};
pub use core::event::{
    AgentEvent, AgentEventEnvelope, CallOutcome, StepStop, StreamEvent, ToolEvent,
    ToolOutputStream, ToolResult, TurnEvent,
};
pub use core::history::{History, Record, SessionMeta};
pub use core::identity::{AgentId, ToolCallId, TurnId};
pub use core::interaction::{
    AnswerResponse, ApprovalDecision, LoopLimitDecision, NO_USER_TO_ANSWER, PendingRequest,
    Question, QuestionOption, RequestSink, UserRequest, UserRequestId, UserResponse,
};
pub use core::mode::{AgentMode, ToolAccess};
pub use core::models::ModelCatalog;
pub use core::prompt_template::{
    InstructionDoc, InstructionScope, MEMORY_PROMPT, load_instructions, subagent_preamble,
    system_prompt,
};
pub use core::selection::{ModelSelection, Selection};
pub use core::sink::{EventSink, NullSink, VecEventSink};
pub use core::subagent::{
    NodeHandle, NodeInfo, NodeOutcome, NodeReport, NodeStatus, RoleSpec, SpawnRequest,
    SubagentSpawner,
};
pub use core::todo::{TodoItem, TodoList, TodoStatus};
pub use core::turn::{
    DEFAULT_MAX_ITERS, PendingPrompts, RunPolicy, Step, StepCall, TurnContext, TurnOutput,
};
pub use core::view::{ToolCaps, ToolPermission, ToolView};
pub use oven_llm::RouterHandle;
pub use runtime::agent::{Agent, router_handle};
pub use runtime::compact::{CompactStats, NOTHING_TO_COMPACT};
pub use tokio_util::sync::CancellationToken;

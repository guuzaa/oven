//! The agent layer: a driver that runs turns against a provider, the tools a
//! turn may call, and the vocabulary both share.
//!
//! Layers, top to bottom; a layer reaches only into the ones below it:
//!
//! 1. `runtime` — the `Agent` driver and the turns it runs
//! 2. `capabilities` — what an agent can call: the `Tool` protocol, the
//!    built-in tools, skills, the protocol for handing work to another agent
//! 3. `core` — the nouns and their pure rules: history and its records, the
//!    turn context, the user-request protocol
//!
//! Below all three sits `oven-proto`: the events, state and commands a
//! frontend shares with the runtime. The agent reports through it, so what a
//! terminal sees and what a web page would see are the same facts.
//!
//! Providers and messages are `oven-llm`; files, processes and the clock are
//! `oven-host`. All of them sit below all three layers.

mod capabilities;
mod core;
mod runtime;

pub use capabilities::skills::{Skill, SkillRegistry};
pub use capabilities::tools::{
    AnswerTool, BUILTIN_TOOLS, BashTool, BuiltinTool, FileEditTool, FileReadTool, FileWriteTool,
    GlobTool, GrepTool, ListModelsTool, SkillReadTool, TaskOutputTool, TaskTool, TodoWriteTool,
    Tool, WebFetchTool, present_tool,
};
pub use core::history::History;
pub use core::interaction::{NO_USER_TO_ANSWER, PendingRequest, RequestSink, UserRequest};
pub use core::models::ModelCatalog;
pub use core::prompt_template::{
    InstructionDoc, InstructionScope, MEMORY_PROMPT, TOP_LEVEL_PROMPT, load_instructions,
    subagent_preamble, system_prompt,
};
pub use core::selection::{ModelSelection, Selection};
pub use core::subagent::{
    NOTICE_MARK, NodeHandle, NodeOutcome, NodeReport, RoleSpec, SpawnRequest, SubagentSpawner,
};
pub use core::turn::{
    DEFAULT_MAX_ITERS, PendingInbox, PendingInput, RunPolicy, Step, StepCall, TurnContext,
    TurnOutput,
};
pub use core::view::{ToolCaps, ToolOutcome};
pub use oven_llm::RouterHandle;
pub use oven_proto::{
    AgentError, AgentEvent, AgentEventEnvelope, AgentId, AgentMode, AnswerResponse,
    ApprovalDecision, CallOutcome, EventSink, LoopLimitDecision, MAX_ITERS_EXCEEDED, NodeInfo,
    NodeStatus, NullSink, Question, QuestionOption, READ_RESULT_HINT, Record, SessionMeta,
    StepStop, StreamEvent, TodoItem, TodoList, TodoStatus, ToolAccess, ToolCallId, ToolEvent,
    ToolOutputStream, ToolPermission, ToolResult, ToolView, TurnEvent, TurnId, UserRequestId,
    UserResponse, VecEventSink,
};
pub use runtime::agent::{Agent, router_handle};
pub use runtime::compact::{CompactStats, NOTHING_TO_COMPACT};
pub use tokio_util::sync::CancellationToken;

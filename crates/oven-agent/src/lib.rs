mod agent;
mod compact;
mod core;
mod skills;
mod tools;

pub use agent::{Agent, RouterHandle, router_handle};
pub use compact::{CompactStats, NOTHING_TO_COMPACT};
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
pub use core::prompt_template::{
    InstructionDoc, InstructionScope, load_instructions, subagent_preamble, system_prompt,
};
pub use core::retry::RetryingProvider;
pub use core::selection::{ModelSelection, Selection};
pub use core::sink::{EventSink, NullSink, VecEventSink};
pub use core::subagent::{
    NodeHandle, NodeInfo, NodeOutcome, NodeReport, NodeStatus, RoleSpec, SpawnRequest,
    SubagentSpawner,
};
pub use core::todo::{TodoItem, TodoList, TodoStatus};
pub use core::turn::{DEFAULT_MAX_ITERS, RunPolicy, Step, StepCall, TurnContext, TurnOutput};
pub use skills::{Skill, SkillRegistry};
pub use tokio_util::sync::CancellationToken;
pub use tools::{
    AnswerTool, BUILTIN_TOOLS, BashTool, BuiltinTool, FileEditTool, FileReadTool, FileWriteTool,
    GlobTool, GrepTool, SkillReadTool, TaskOutputTool, TaskTool, TodoWriteTool, Tool, ToolCaps,
    ToolPermission, ToolView, present_tool,
};

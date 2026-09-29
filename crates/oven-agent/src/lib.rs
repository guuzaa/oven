mod agent;
mod approval;
mod compact;
mod error;
mod event;
mod history;
mod identity;
mod matching;
mod mode;
mod prompt_template;
mod question;
mod retry;
mod sink;
mod skills;
mod subagent;
mod todo;
mod tools;
mod turn;

pub use agent::{Agent, RouterHandle, router_handle};
pub use approval::{
    ApprovalDecision, ApprovalRequestId, ApprovalSender, LoopLimitDecision, LoopLimitPrompt,
    LoopLimitRequestId, LoopLimitSender, ToolApproval,
};
pub use compact::{CompactStats, NOTHING_TO_COMPACT};
pub use error::{AgentError, MAX_ITERS_EXCEEDED};
pub use event::{
    AgentEvent, AgentEventEnvelope, CallOutcome, StepStop, StreamEvent, ToolEvent,
    ToolOutputStream, ToolResult, TurnEvent,
};
pub use history::{History, Record, SessionMeta};
pub use identity::{AgentId, ToolCallId, TurnId};
pub use mode::{AgentMode, ToolAccess};
pub use prompt_template::{
    InstructionDoc, InstructionScope, load_instructions, subagent_preamble, system_prompt,
};
pub use question::{
    AnswerResponse, NO_USER_TO_ANSWER, Question, QuestionOption, QuestionRequest,
    QuestionRequestId, QuestionSender,
};
pub use retry::RetryingProvider;
pub use sink::{ChannelEventSink, EventSink, NullSink, VecEventSink};
pub use skills::{Skill, SkillRegistry};
pub use subagent::{
    NodeHandle, NodeInfo, NodeOutcome, NodeReport, NodeStatus, RoleSpec, SpawnRequest,
    SubagentSpawner,
};
pub use todo::{TodoItem, TodoList, TodoStatus};
pub use tokio_util::sync::CancellationToken;
pub use tools::{
    AnswerTool, BUILTIN_TOOLS, BashTool, BuiltinTool, FileEditTool, FileReadTool, FileWriteTool,
    GlobTool, GrepTool, SkillReadTool, TaskOutputTool, TaskTool, TodoWriteTool, Tool, ToolCaps,
    ToolPermission, ToolView, present_tool,
};
pub use turn::{DEFAULT_MAX_ITERS, RunPolicy, Step, StepCall, TurnContext, TurnOutput};

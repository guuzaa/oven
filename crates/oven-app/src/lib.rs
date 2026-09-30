mod app;
mod builder;
mod command;
mod core;
mod inbox;
pub mod mcp;
mod platform;
mod runtime;
mod shared;
mod slash;
mod subagent;
mod tools;

pub use app::App;
pub use builder::AppBuilder;
pub use command::Input;
pub use core::error::AppError;
pub use core::event::{AppEvent, AppEventKind, AppId, CompactionEvent, ShellEvent, SubagentEvent};
pub use core::mention::FileMentions;
pub use core::session;
pub use core::state::{AppPhase, AppState, HistoryChangeReason, SessionState, context_tokens_of};
pub use core::{complete, config};
pub use mcp::McpServerConfig;
pub use oven_agent::{
    AgentEvent, AgentEventEnvelope, AgentId, AgentMode, AnswerResponse, AnswerTool,
    ApprovalDecision, CallOutcome, CancellationToken, LoopLimitDecision, NodeInfo, NodeStatus,
    Question, QuestionOption, RoleSpec, Skill, SkillRegistry, StepStop, StreamEvent, TodoItem,
    TodoList, TodoStatus, ToolCallId, ToolEvent, ToolResult, ToolView, TurnEvent, TurnId,
    UserRequestId, UserResponse, present_tool,
};
pub use platform::shell::{LocalShell, ShellInput, display_shell_line};
pub use platform::{dirs, log};
pub use tools::ToolRegistry;

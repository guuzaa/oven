mod app;
mod builder;
mod capabilities;
mod command;
mod commands;
mod core;
mod platform;
mod runtime;

pub use app::App;
pub use builder::AppBuilder;
pub use capabilities::mcp;
pub use capabilities::tools::ToolRegistry;
pub use command::Input;
pub use core::config::McpServerConfig;
pub use core::error::AppError;
pub use core::event::{AppEvent, AppEventKind, AppId, CompactionEvent, ShellEvent, SubagentEvent};
pub use core::mention::FileMentions;
pub use core::session;
pub use core::state::{AppPhase, AppState, HistoryChangeReason, SessionState, context_tokens_of};
pub use core::{complete, config};
pub use oven_agent::{
    AgentEvent, AgentEventEnvelope, AgentId, AgentMode, AnswerResponse, AnswerTool,
    ApprovalDecision, CallOutcome, CancellationToken, LoopLimitDecision, NodeInfo, NodeStatus,
    Question, QuestionOption, RoleSpec, Skill, SkillRegistry, StepStop, StreamEvent, TodoItem,
    TodoList, TodoStatus, ToolCallId, ToolEvent, ToolResult, ToolView, TurnEvent, TurnId,
    UserRequestId, UserResponse, present_tool,
};
pub use platform::shell::{LocalShell, ShellInput, display_shell_line};
pub use platform::{dirs, log};

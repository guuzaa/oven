mod app;
mod builder;
mod command;
pub mod config;
pub mod dirs;
mod event;
pub mod mcp;
mod mention;
mod prompt_template;
mod provider;
mod runtime;
pub mod session;
mod shell;
mod slash;
mod state;
mod tools;

pub use app::{App, AppError};
pub use builder::AppBuilder;
pub use command::AppCommand;
pub use event::{AppEvent, AppEventKind, AppId, ShellEvent};
pub use mcp::McpServerConfig;
pub use mention::FileMentions;
pub use oven_agent::{
    AgentEvent, AgentEventEnvelope, AgentId, AgentMode, CancellationToken, Skill, SkillRegistry,
    StreamEvent, TodoItem, TodoList, TodoStatus, ToolCallId, ToolEvent, ToolResult, ToolView,
    TurnEvent, TurnId, present_tool,
};
pub use shell::{LocalShell, ShellInput, display_shell_line};
pub use state::{AppPhase, AppState, SessionState, StateChange, StateEvent};
pub use tools::ToolRegistry;

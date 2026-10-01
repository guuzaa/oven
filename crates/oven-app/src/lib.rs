//! Service composition behind the `App` facade.
//!
//! Layers, top to bottom; a layer reaches only into the ones below it:
//!
//! 1. `api` — the `App` handle a frontend talks to, and its builder
//! 2. `runtime` — the conversation loop and the state shared with it
//! 3. `commands` — slash commands
//! 4. `capabilities` — tools, subagents, MCP servers
//! 5. `core` — config, state, events, persistence
//! 6. `platform` — paths, logging, the local shell envelope
//!
//! One edge moves the other way: the runtime also assembles the `App`
//! handle the facade hands out.

mod api;
mod capabilities;
mod commands;
mod core;
mod platform;
mod runtime;

pub use api::{App, AppBuilder};
pub use capabilities::mcp;
pub use capabilities::tools::ToolRegistry;
pub use core::config::McpServerConfig;
pub use core::error::AppError;
pub use core::event::{AppEvent, AppEventKind, AppId, CompactionEvent, ShellEvent, SubagentEvent};
pub use core::input::Input;
pub use core::mention::FileMentions;
pub use core::provider::{provider_catalog, provider_models, verify};
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

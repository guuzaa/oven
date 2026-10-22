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
//!
//! Below all of them sits `oven-proto`: the events, state and commands a
//! frontend shares with the runtime. This crate speaks it; it does not own it.

mod api;
mod capabilities;
mod commands;
mod core;
mod platform;
mod runtime;

pub use api::{App, AppBuilder};
pub use capabilities::mcp;
pub use capabilities::tools::ToolRegistry;
pub use capabilities::view::present_tool;
pub use core::config::McpServerConfig;
pub use core::error::AppError;
pub use core::mention::FileMentions;
pub use core::provider::{provider_catalog, provider_models, verify};
pub use core::session;
pub use core::{complete, config};
pub use oven_agent::{AnswerTool, CancellationToken, NOTICE_MARK, RoleSpec, Skill, SkillRegistry};
pub use oven_proto::{
    AgentEvent, AgentEventEnvelope, AgentId, AgentMode, AnswerResponse, AppEvent, AppEventKind,
    AppId, AppLevels, AppPhase, AppState, ApprovalDecision, COMPACTION_CANCELLED, CallOutcome,
    Command, CommandResult, CompactionEvent, HistoryChangeReason, Input, LoopLimitDecision,
    NodeInfo, NodeStatus, ProviderView, Question, QuestionOption, SessionState, ShellEvent,
    StepStop, StreamEvent, SubagentEvent, TodoItem, TodoList, TodoStatus, ToolCallId, ToolEvent,
    ToolResult, ToolView, TurnEvent, TurnId, UserRequestId, UserResponse,
};
pub use platform::shell::{LocalShell, ShellInput, display_shell_line};
pub use platform::{dirs, log};

pub mod memory;

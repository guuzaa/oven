//! Compile-time pins for the paths `oven-app` exposes: moving a module behind
//! a new layer must not move a public path.

#![allow(unused_imports)]

use oven_app::{
    App, AppBuilder, AppError, AppEvent, AppEventKind, AppId, AppPhase, AppState, CompactionEvent,
    FileMentions, HistoryChangeReason, Input, LocalShell, McpServerConfig, SessionState,
    ShellEvent, ShellInput, SubagentEvent, ToolRegistry, display_shell_line,
};

use oven_app::complete;
use oven_app::config::{self, AppConfig, ProviderConfig, ProviderSelection};
use oven_app::dirs;
use oven_app::log;
use oven_app::mcp::client::{DefaultMcpConnector, McpCaller, McpConnector, McpTool};
use oven_app::mcp::{McpError, McpRegistry};
use oven_app::session::{self, Session};

use oven_app::{
    AgentEvent, AgentEventEnvelope, AgentId, AgentMode, AnswerResponse, AnswerTool,
    ApprovalDecision, CallOutcome, CancellationToken, LoopLimitDecision, NodeInfo, NodeStatus,
    Question, QuestionOption, RoleSpec, Skill, SkillRegistry, StepStop, StreamEvent, TodoItem,
    TodoList, TodoStatus, ToolCallId, ToolEvent, ToolResult, ToolView, TurnEvent, TurnId,
    UserRequestId, UserResponse, present_tool,
};

#[test]
fn public_paths_resolve() {
    assert!(oven_app::dirs::logs_dir().is_some() || oven_app::dirs::logs_dir().is_none());
    assert!(complete::matches("model", "mo"));
}

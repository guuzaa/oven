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
    ApprovalDecision, BUILTIN_TOOLS, BashTool, BuiltinTool, CallOutcome, CancellationToken,
    FileEditTool, FileReadTool, FileWriteTool, GlobTool, GrepTool, ListModelsTool,
    LoopLimitDecision, NodeInfo, NodeStatus, Question, QuestionOption, RoleSpec, SKILL_FILE, Skill,
    SkillRegistry, StepStop, StreamEvent, TaskOutputTool, TaskTool, TodoItem, TodoList, TodoStatus,
    TodoWriteTool, ToolCallId, ToolEvent, ToolResult, ToolView, TurnEvent, TurnId, UserRequestId,
    UserResponse, WebFetchTool, present_tool,
};

#[test]
fn public_paths_resolve() {
    assert!(oven_app::dirs::logs_dir().is_some() || oven_app::dirs::logs_dir().is_none());
    assert!(complete::matches("model", "mo"));

    assert_eq!(SKILL_FILE, "SKILL.md");

    assert_eq!(
        present_tool(
            FileReadTool::NAME,
            &serde_json::json!({ "path": "src/lib.rs" })
        )
        .summary,
        "Read src/lib.rs"
    );

    assert!(!BUILTIN_TOOLS.is_empty());
    assert_eq!(BUILTIN_TOOLS[0].name, FileReadTool::NAME);
    assert_eq!(WebFetchTool::NAME, "web_fetch");
    assert!(
        BUILTIN_TOOLS
            .iter()
            .any(|tool| tool.name == WebFetchTool::NAME)
    );
}

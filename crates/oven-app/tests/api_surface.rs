//! Temporary guard: every public path `oven-app` exposed before the layering move.
//! Deleted once the move is verified.

#[allow(unused_imports)]
mod types {
    use oven_app::{
        App, AppBuilder, AppError, AppEvent, AppEventKind, AppId, AppPhase, AppState,
        CompactionEvent, FileMentions, HistoryChangeReason, Input, LocalShell, McpServerConfig,
        SessionState, ShellEvent, ShellInput, SubagentEvent, ToolRegistry, context_tokens_of,
        display_shell_line,
    };
}

#[allow(unused_imports)]
mod modules {
    use oven_app::complete;
    use oven_app::config::{self, AppConfig, ProviderConfig, ProviderSelection};
    use oven_app::dirs;
    use oven_app::log;
    use oven_app::mcp::client::{DefaultMcpConnector, McpCaller, McpConnector, McpTool};
    use oven_app::mcp::{McpError, McpRegistry};
    use oven_app::session::{self, Session};

    #[allow(dead_code)]
    fn probe() {
        let _ = complete::matches("model", "mo");
        let _ = dirs::logs_dir();
        let _ = oven_app::session::canonical_root;
    }
}

#[allow(unused_imports)]
mod reexports {
    use oven_app::{
        AgentEvent, AgentEventEnvelope, AgentId, AgentMode, AnswerResponse, AnswerTool,
        ApprovalDecision, CallOutcome, CancellationToken, LoopLimitDecision, NodeInfo, NodeStatus,
        Question, QuestionOption, RoleSpec, Skill, SkillRegistry, StepStop, StreamEvent, TodoItem,
        TodoList, TodoStatus, ToolCallId, ToolEvent, ToolResult, ToolView, TurnEvent, TurnId,
        UserRequestId, UserResponse, present_tool,
    };
}

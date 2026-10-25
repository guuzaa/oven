//! Compile-time pins for the paths `oven-proto` exposes: moving a module
//! behind a new layer must not move a public path.

#![allow(unused_imports)]

use oven_proto::{
    AgentError, AgentEvent, AgentEventEnvelope, AgentId, AgentMode, AnswerResponse,
    ApprovalDecision, CallOutcome, Command, CommandResult, EventSink, LoopLimitDecision,
    MAX_ITERS_EXCEEDED, NodeInfo, NodeStatus, NullSink, Question, QuestionOption, READ_RESULT_HINT,
    Record, SessionMeta, StepStop, StreamEvent, TodoItem, TodoList, TodoStatus, ToolAccess,
    ToolCallId, ToolEvent, ToolOutputStream, ToolPermission, ToolResult, ToolView, TurnEvent,
    TurnId, UserRequestId, UserResponse, VecEventSink,
};

#[test]
fn public_paths_resolve() {
    let mut sink = VecEventSink::default();
    sink.emit(AgentEvent::Usage {
        usage: oven_llm::Usage::default(),
    });
    assert_eq!(sink.events.len(), 1);

    let record = Record::TodoList {
        timestamp: 1,
        items: Vec::new(),
    };
    assert!(matches!(record, Record::TodoList { .. }));

    assert_eq!(ToolPermission::default(), ToolPermission::Read);
    assert_eq!(
        AgentMode::default().tool_access(ToolPermission::Write),
        ToolAccess::Allowed
    );
    assert_eq!(
        AgentMode::Ask.tool_access(ToolPermission::Write),
        ToolAccess::Hidden
    );
    assert_eq!(NodeStatus::Pending.label(), "queued");

    assert!(AgentId::next() < AgentId::next());
    assert_eq!(MAX_ITERS_EXCEEDED, "agent loop exceeded max iterations");
}

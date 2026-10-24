//! Compile-time pins for the paths `oven-agent` exposes: moving a module behind
//! a new layer must not move a public path.

#![allow(unused_imports)]

use oven_agent::{
    Agent, AgentEvent, AgentEventEnvelope, AgentId, AgentMode, AnswerResponse, ApprovalDecision,
    CallOutcome, CancellationToken, CompactStats, DEFAULT_MAX_ITERS, EventSink, History,
    InstructionDoc, InstructionScope, LoopLimitDecision, MAX_ITERS_EXCEEDED, ModelCatalog,
    ModelSelection, NO_USER_TO_ANSWER, NOTHING_TO_COMPACT, NOTICE_MARK, NodeHandle, NodeInfo,
    NodeOutcome, NodeReport, NodeStatus, NullSink, PendingInbox, PendingInput, PendingRequest,
    Question, QuestionOption, Record, RequestSink, RoleSpec, RouterHandle, RunPolicy, Selection,
    SessionMeta, SpawnRequest, Step, StepCall, StepStop, StreamEvent, SubagentSpawner, Tool,
    ToolAccess, ToolCallId, ToolCaps, ToolEffect, ToolEvent, ToolOutcome, ToolOutputStream,
    ToolPermission, ToolResult, ToolView, TurnContext, TurnEvent, TurnId, TurnOutput, UserRequest,
    UserRequestId, UserResponse, VecEventSink,
};

#[test]
fn public_paths_resolve() {
    let mut sink = VecEventSink::default();
    sink.emit(AgentEvent::Usage {
        usage: oven_llm::Usage::default(),
    });
    assert_eq!(sink.events.len(), 1);

    assert!(History::new().is_empty());
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
    assert_eq!(DEFAULT_MAX_ITERS, 200);
    assert_eq!(MAX_ITERS_EXCEEDED, "agent loop exceeded max iterations");
    assert_eq!(NOTHING_TO_COMPACT, "nothing to compact");
    assert_eq!(
        NO_USER_TO_ANSWER,
        "no user is available to answer the question"
    );
}

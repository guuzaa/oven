//! Compile-time pins for the paths `oven-agent` exposes: moving a module behind
//! a new layer must not move a public path.

#![allow(unused_imports)]

use oven_agent::{
    Agent, CancellationToken, CompactStats, DEFAULT_MAX_ITERS, History, InstructionDoc,
    InstructionScope, ModelCatalog, ModelSelection, NO_USER_TO_ANSWER, NOTHING_TO_COMPACT,
    NOTICE_MARK, NodeHandle, NodeOutcome, NodeReport, PendingInbox, PendingInput, PendingRequest,
    RequestSink, RoleSpec, RouterHandle, RunPolicy, Selection, SpawnRequest, Step, StepCall,
    SubagentSpawner, Tool, ToolCaps, ToolEffect, ToolOutcome, TurnContext, TurnOutput, UserRequest,
};

#[test]
fn public_paths_resolve() {
    assert!(History::new().is_empty());

    assert_eq!(DEFAULT_MAX_ITERS, 200);
    assert_eq!(NOTHING_TO_COMPACT, "nothing to compact");
    assert_eq!(
        NO_USER_TO_ANSWER,
        "no user is available to answer the question"
    );
}

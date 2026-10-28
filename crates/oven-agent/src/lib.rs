//! The agent layer: a driver that runs turns against a provider, the
//! protocol the tools of a turn speak, and the vocabulary both share.
//!
//! Layers, top to bottom; a layer reaches only into the ones below it:
//!
//! 1. `runtime` — the `Agent` driver and the turns it runs
//! 2. `core` — the nouns and their pure rules: history and its records, the
//!    turn context, the user-request protocol, the tool protocol
//!
//! Below both sits `oven-proto`: the events, state and commands a
//! frontend shares with the runtime. The agent reports through it, so what a
//! terminal sees and what a web page would see are the same facts. That
//! vocabulary is not re-exported here: a crate that names an event names
//! `oven-proto` for it.
//!
//! Providers and messages are `oven-llm`; files, processes and the clock are
//! `oven-host`. All of them sit below both layers.

mod core;
mod runtime;

pub use core::history::History;
pub use core::interaction::{NO_USER_TO_ANSWER, PendingRequest, RequestSink, UserRequest};
pub use core::models::ModelCatalog;
pub use core::prompt_template::{
    InstructionDoc, InstructionScope, MEMORY_PROMPT, TOP_LEVEL_PROMPT, load_instructions,
    subagent_preamble, system_prompt,
};
pub use core::selection::{ModelSelection, Selection};
pub use core::subagent::{
    NOTICE_MARK, NodeHandle, NodeOutcome, NodeReport, RoleSpec, SpawnRequest, SubagentSpawner,
};
pub use core::tool::Tool;
pub use core::turn::{
    DEFAULT_MAX_ITERS, PendingInbox, PendingInput, RunPolicy, Step, StepCall, TurnContext,
    TurnOutput,
};
pub use core::view::{ToolCaps, ToolEffect, ToolOutcome};
pub use oven_llm::RouterHandle;
pub use runtime::agent::{Agent, REPLY_CUT_OFF, REPLY_EMPTY, router_handle};
pub use runtime::compact::{CompactStats, NOTHING_TO_COMPACT};
pub use tokio_util::sync::CancellationToken;

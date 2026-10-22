//! The protocol for handing work to another agent.
//!
//! What a subagent *is* — its status, its progress, its address — belongs to
//! `oven-proto`, because a frontend tracks it. What stays here is the
//! machinery: the spawn request, the handle a caller waits on, and the
//! [`SubagentSpawner`] whoever implements it offers. The app layer supplies
//! that implementation, so a frontend can list, cancel and read subagents
//! without knowing how they are built.

use async_trait::async_trait;
use oven_llm::{ContentBlock, Message, ModelId, ReasoningEffort, Role, Usage};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use oven_proto::{AgentError, AgentId, NodeInfo, NodeStatus, progress};

/// Marks a completion notice in the history, so a frontend can tell it from
/// something the user typed once a session is reloaded. It is the only
/// handle a reloaded transcript has: a message carries no other origin, so the
/// mark is written and read through [`notice_message`] and [`is_notice`]
/// rather than spelled out anywhere else.
pub const NOTICE_MARK: &str = "[subagent notice]";

/// The user message a completion notice rides in: the model reads it as
/// something that happened while it worked, and a reloaded transcript reads
/// the mark back to tell it from a typed prompt.
pub(crate) fn notice_message(text: &str) -> Message {
    Message::user_text(format!("{NOTICE_MARK} {text}"))
}

/// Whether `message` is a completion notice rather than something the user
/// typed.
pub(crate) fn is_notice(message: &Message) -> bool {
    message.role == Role::User
        && message.content.iter().any(
            |block| matches!(block, ContentBlock::Text { text } if text.starts_with(NOTICE_MARK)),
        )
}

/// A role a subagent can play: which tools it may use, and what it is told
/// to do with them. The name is what the model asks for and what the
/// frontend lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSpec {
    pub name: String,
    pub description: String,
    /// Roles that only read are the ones plan mode may run.
    pub read_only: bool,
}

/// A subagent plus the report it produced, as `task_output` wants them.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeReport {
    pub info: NodeInfo,
    pub report: Option<String>,
}

/// Everything a spawner needs to run one subagent. The model supplies the
/// first four fields; the caller supplies the run it belongs to.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub role: String,
    pub label: String,
    pub prompt: String,
    pub background: bool,
    pub model: ModelId,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub cancellation: CancellationToken,
}

/// What a subagent produced, as the caller that waited on it sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeOutcome {
    pub status: NodeStatus,
    pub report: String,
    pub tool_calls: u32,
    pub steps: u32,
    pub usage: Usage,
    pub duration_ms: u64,
}

impl NodeOutcome {
    /// What a caller that waited on this subagent reads before its report.
    pub fn progress(&self) -> String {
        progress(
            self.status.label(),
            self.steps,
            self.tool_calls,
            self.usage,
            self.duration_ms,
        )
    }
}

/// A spawned subagent. A caller that wants the result waits on it; a caller
/// that does not drops it and polls [`SubagentSpawner::reports`] later.
pub struct NodeHandle {
    pub id: AgentId,
    pub name: String,
    outcome: oneshot::Receiver<NodeOutcome>,
}

impl NodeHandle {
    pub fn new(id: AgentId, name: String, outcome: oneshot::Receiver<NodeOutcome>) -> Self {
        Self { id, name, outcome }
    }

    /// Waits for the subagent's turn to end. Returns `cancelled` when the
    /// subagent went away without reporting, which is what a caller that
    /// cancelled it should treat as its own cancellation.
    pub async fn join(self) -> Result<NodeOutcome, AgentError> {
        self.outcome.await.map_err(|_| AgentError::cancelled())
    }
}

/// Runs subagents and reports on the ones it has run.
#[async_trait]
pub trait SubagentSpawner: Send + Sync {
    async fn spawn(&self, request: SpawnRequest) -> Result<NodeHandle, AgentError>;

    /// Report on `name`, or on every subagent when it is `None`. Unknown
    /// names are an error so the model learns it guessed wrong.
    fn reports(&self, name: Option<&str>) -> Result<Vec<NodeReport>, AgentError>;
}

//! The protocol for handing work to another agent.
//!
//! `oven-agent` owns the vocabulary — what a subagent is, what it reports
//! back, how it is addressed — while whoever implements
//! [`SubagentSpawner`] owns the machinery: which tools a role mounts, how
//! many may run at once, and how they are cancelled. The app layer supplies
//! that implementation, so a frontend can list, cancel and read subagents
//! without knowing how they are built.

use async_trait::async_trait;
use oven_llm::{ModelId, ReasoningEffort, Usage};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::core::error::AgentError;
use crate::core::identity::{AgentId, TurnId};

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

/// What a subagent is doing, from the moment it is created to the moment it
/// stops. A subagent runs its own turn, so it never enters the caller's
/// phase: this is where its progress lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeStatus {
    /// Created, waiting for a concurrency slot.
    Pending,
    Running {
        turn_id: TurnId,
    },
    Completed,
    Failed {
        error: String,
    },
    Cancelled,
}

impl NodeStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Pending | Self::Running { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Pending => "queued",
            Self::Running { .. } => "running",
            Self::Completed => "done",
            Self::Failed { .. } => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A subagent as the frontend tracks it: status metadata cheap enough to
/// mirror into published state on every change. The report itself stays in
/// the spawner until someone asks for it.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeInfo {
    pub id: AgentId,
    /// Stable address of this subagent, e.g. `explore#2`. Unique per app run.
    pub name: String,
    pub role: String,
    /// Short human label taken from the request that spawned it.
    pub label: String,
    pub background: bool,
    /// The agent that spawned it. With one level of nesting this is always
    /// the root agent, but the edge is what lets a caller rebuild the tree.
    pub parent: AgentId,
    pub status: NodeStatus,
    pub usage: Usage,
    pub tool_calls: u32,
    /// Provider round trips finished so far.
    pub steps: u32,
    pub started_at: u64,
    /// Unix ms when it stopped. `None` while it is still going, which is what
    /// keeps a stopped subagent's clock from running on.
    pub finished_at: Option<u64>,
}

impl NodeInfo {
    /// How long it has been running: until it stopped, or until now. A
    /// subagent that was cancelled or failed reports the time it took, not
    /// the time since it started.
    pub fn elapsed_ms(&self) -> u64 {
        self.finished_at
            .unwrap_or_else(oven_host::now_ms)
            .saturating_sub(self.started_at)
    }
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
    /// The caller's turn, which a foreground subagent's lifetime follows.
    pub parent_turn: TurnId,
    pub cancellation: CancellationToken,
}

/// What a subagent produced, as the caller that waited on it sees it.
#[derive(Debug, Clone)]
pub struct NodeOutcome {
    pub status: NodeStatus,
    pub report: String,
    pub tool_calls: u32,
    pub steps: u32,
    pub usage: Usage,
    pub duration_ms: u64,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn info(status: NodeStatus, started_at: u64, finished_at: Option<u64>) -> NodeInfo {
        NodeInfo {
            id: AgentId::next(),
            name: "explore#1".into(),
            role: "explore".into(),
            label: String::new(),
            background: false,
            parent: AgentId::next(),
            status,
            usage: Usage::default(),
            tool_calls: 0,
            steps: 0,
            started_at,
            finished_at,
        }
    }

    #[test]
    fn a_stopped_subagent_stops_counting() {
        let stopped = info(NodeStatus::Cancelled, 1_000, Some(4_500));
        assert_eq!(stopped.elapsed_ms(), 3_500);

        let running = info(
            NodeStatus::Running {
                turn_id: TurnId::next(),
            },
            oven_host::now_ms().saturating_sub(250),
            None,
        );
        assert!(
            running.elapsed_ms() >= 250,
            "a running subagent counts up: {}",
            running.elapsed_ms()
        );
    }

    #[test]
    fn active_covers_pending_and_running() {
        assert!(NodeStatus::Pending.is_active());
        assert!(
            NodeStatus::Running {
                turn_id: TurnId::next()
            }
            .is_active()
        );
        assert!(!NodeStatus::Completed.is_active());
        assert!(!NodeStatus::Cancelled.is_active());
        assert!(!NodeStatus::Failed { error: "x".into() }.is_active());
    }
}

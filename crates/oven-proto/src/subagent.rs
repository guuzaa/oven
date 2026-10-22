//! A subagent as the frontend tracks it.
//!
//! The report a subagent produced stays with whoever spawned it until
//! someone asks for it; what the frontend needs is the status metadata, cheap
//! enough to mirror into published state on every change.

use oven_llm::Usage;
use serde::{Deserialize, Serialize};

use crate::{AgentId, TurnId};

/// What a subagent is doing, from the moment it is created to the moment it
/// stops. A subagent runs its own turn, so it never enters the caller's
/// phase: this is where its progress lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

/// A subagent as the frontend tracks it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// How long it has been running: until it stopped, or until `now_ms`.
    /// A subagent that was cancelled or failed reports the time it took, not
    /// the time since it started.
    pub fn elapsed_ms(&self, now_ms: u64) -> u64 {
        self.finished_at
            .unwrap_or(now_ms)
            .saturating_sub(self.started_at)
    }

    /// What this subagent has done so far: status, provider round trips,
    /// tool calls, tokens and time. `task_output` renders it and a
    /// completion notice quotes it, so the two never describe the same
    /// subagent differently.
    pub fn progress(&self, now_ms: u64) -> String {
        progress(
            self.status.label(),
            self.steps,
            self.tool_calls,
            self.usage,
            self.elapsed_ms(now_ms),
        )
    }

    /// The one line a caller that is not waiting on this subagent reads:
    /// what finished, what it spent, and how to read the rest.
    pub fn notice(&self, now_ms: u64) -> String {
        format!(
            "{}: {}; {READ_RESULT_HINT}.",
            self.name,
            self.progress(now_ms)
        )
    }
}

/// How a caller that is not waiting on this subagent reads the rest of what
/// it did. `task` points at it before a background subagent has finished, a
/// notice quotes it after.
pub const READ_RESULT_HINT: &str = "read its result with task_output";

pub fn progress(
    status: &str,
    steps: u32,
    tool_calls: u32,
    usage: Usage,
    duration_ms: u64,
) -> String {
    let tally = match steps {
        0 => "not started".to_string(),
        _ => format!("{steps} steps, {tool_calls} tool calls"),
    };
    // Cache reads are a subset of the input tokens, not extra ones: a
    // provider charges for the prompt once and serves part of it from cache.
    let tokens = usage.input_tokens.saturating_add(usage.output_tokens);
    format!(
        "{status} · {tally} · {tokens} tokens · {:.1}s",
        duration_ms as f64 / 1000.0
    )
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
        assert_eq!(stopped.elapsed_ms(9_000), 3_500);
    }

    #[test]
    fn a_running_subagent_counts_up() {
        let running = info(
            NodeStatus::Running {
                turn_id: TurnId::next(),
            },
            1_000,
            None,
        );
        assert_eq!(running.elapsed_ms(1_250), 250);
    }

    #[test]
    fn the_info_survives_the_wire() {
        let node = info(NodeStatus::Pending, 1, None);
        let json = serde_json::to_string(&node).unwrap();
        assert_eq!(serde_json::from_str::<NodeInfo>(&json).unwrap(), node);
    }
}

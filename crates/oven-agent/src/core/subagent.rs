//! The protocol for handing work to another agent.
//!
//! `oven-agent` owns the vocabulary — what a subagent is, what it reports
//! back, how it is addressed — while whoever implements
//! [`SubagentSpawner`] owns the machinery: which tools a role mounts, how
//! many may run at once, and how they are cancelled. The app layer supplies
//! that implementation, so a frontend can list, cancel and read subagents
//! without knowing how they are built.

use async_trait::async_trait;
use oven_llm::{ContentBlock, Message, ModelId, ReasoningEffort, Role, Usage};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::core::error::AgentError;
use crate::core::identity::{AgentId, TurnId};

/// Marks a completion notice in the history, so a frontend can tell it from
/// something the user typed once a session is reloaded. It is the only
/// handle a reloaded transcript has: a message carries no other origin, so
/// the mark is written and read through [`notice_message`] and [`is_notice`]
/// rather than spelled out anywhere else.
pub const NOTICE_MARK: &str = "[subagent notice]";

/// How a caller reads the rest of what a subagent did. `task` points at it
/// before a background subagent has finished, a notice quotes it after.
pub const READ_RESULT_HINT: &str = "read its result with task_output";

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

    /// What this subagent has done so far: status, provider round trips,
    /// tool calls, tokens and time. `task_output` renders it and a
    /// completion notice quotes it, so the two never describe the same
    /// subagent differently.
    pub fn progress(&self) -> String {
        progress(
            self.status.label(),
            self.steps,
            self.tool_calls,
            self.usage,
            self.elapsed_ms(),
        )
    }

    /// The one line a caller that is not waiting on this subagent reads:
    /// what finished, what it spent, and how to read the rest.
    pub fn notice(&self) -> String {
        format!("{}: {}; {READ_RESULT_HINT}.", self.name, self.progress())
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

fn progress(status: &str, steps: u32, tool_calls: u32, usage: Usage, duration_ms: u64) -> String {
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

    fn usage(input_tokens: u32, output_tokens: u32) -> Usage {
        Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
        }
    }

    #[test]
    fn progress_counts_the_reported_tokens_once() {
        let mut reported = usage(1000, 40);
        reported.cache_read_tokens = 500;
        let outcome = NodeOutcome {
            status: NodeStatus::Completed,
            report: String::new(),
            tool_calls: 6,
            steps: 3,
            usage: reported,
            duration_ms: 32_400,
        };
        assert_eq!(
            outcome.progress(),
            "done · 3 steps, 6 tool calls · 1040 tokens · 32.4s"
        );
    }

    #[test]
    fn a_subagent_that_never_ran_says_so() {
        let mut queued = info(NodeStatus::Pending, 0, Some(0));
        queued.steps = 0;
        queued.tool_calls = 0;
        queued.usage = usage(1000, 40);
        assert_eq!(
            queued.progress(),
            "queued · not started · 1040 tokens · 0.0s"
        );
    }

    #[test]
    fn a_notice_quotes_the_progress_and_points_at_task_output() {
        let mut done = info(NodeStatus::Completed, 0, Some(1_000));
        done.steps = 3;
        done.tool_calls = 6;
        done.usage = usage(1000, 40);
        assert_eq!(
            done.notice(),
            format!("explore#1: {}; {READ_RESULT_HINT}.", done.progress())
        );
    }

    #[test]
    fn a_notice_message_round_trips_through_the_mark() {
        let message = notice_message("explore#1 finished");
        assert!(is_notice(&message));
        assert_eq!(message.role, Role::User);
        assert!(!is_notice(&Message::user_text("explore#1 finished")));
        assert!(!is_notice(&Message::assistant_text(format!(
            "{NOTICE_MARK} quoted"
        ))));
    }
}

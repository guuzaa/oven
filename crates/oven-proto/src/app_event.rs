//! Everything a frontend can see happening.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::{AgentEvent, AgentEventEnvelope, AgentId, HistoryChangeReason, TurnId, UserRequestId};

/// The app a frontend is talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct AppId(pub u64);

impl AppId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEvent {
    pub kind: AppEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum AppEventKind {
    Agent(AgentEventEnvelope),
    /// The user request announced earlier is closed: it was answered, or the
    /// turn dropped it. A frontend closes the prompt it opened for this id.
    RequestResolved {
        request_id: UserRequestId,
    },
    Subagent(SubagentEvent),
    /// The history was replaced wholesale, so a view rebuilds itself from
    /// the state instead of following per-message events.
    HistoryChanged {
        reason: HistoryChangeReason,
    },
    Shell(ShellEvent),
    Compaction(CompactionEvent),
    Notification {
        text: String,
    },
    Error {
        message: String,
    },
    Exited,
}

/// Something a frontend should do about subagents, as opposed to what they
/// are, which is [`AppLevels::subagents`](crate::AppLevels::subagents).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubagentEvent {
    /// Open a view on one subagent.
    Focus { id: AgentId },
}

pub const COMPACTION_CANCELLED: &str = "compaction cancelled";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompactionEvent {
    Started,
    Completed {
        before_tokens: u32,
        after_tokens: u32,
    },
    Failed {
        error: String,
    },
    /// The summary request was aborted. History and the session file are unchanged.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShellEvent {
    Started {
        command: String,
    },
    Finished {
        command: String,
        output: String,
        exit_code: i32,
    },
    Failed {
        command: String,
        error: String,
        output: String,
    },
}

impl AppEvent {
    pub fn new(kind: AppEventKind) -> Self {
        Self { kind }
    }

    pub fn notification(text: impl Into<String>) -> Self {
        Self::new(AppEventKind::Notification { text: text.into() })
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(AppEventKind::Error {
            message: message.into(),
        })
    }

    pub fn exited() -> Self {
        Self::new(AppEventKind::Exited)
    }

    pub fn shell(event: ShellEvent) -> Self {
        Self::new(AppEventKind::Shell(event))
    }

    pub fn subagent(event: SubagentEvent) -> Self {
        Self::new(AppEventKind::Subagent(event))
    }

    pub fn compaction(event: CompactionEvent) -> Self {
        Self::new(AppEventKind::Compaction(event))
    }

    pub fn agent(event: AgentEventEnvelope) -> Self {
        Self::new(AppEventKind::Agent(event))
    }

    pub fn agent_with(agent_id: AgentId, turn_id: TurnId, event: AgentEvent) -> Self {
        Self::new(AppEventKind::Agent(AgentEventEnvelope {
            agent_id,
            turn_id,
            event,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_event_survives_the_wire() {
        let event = AppEvent::notification("model switched");
        let json = serde_json::to_string(&event.kind).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"notification","data":{"text":"model switched"}}"#
        );
        assert_eq!(
            serde_json::from_str::<AppEventKind>(&json).unwrap(),
            event.kind
        );
    }
}

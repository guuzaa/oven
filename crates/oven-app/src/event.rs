use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use oven_agent::{AgentEvent, AgentEventEnvelope, AgentId, TurnId};
use tokio::sync::mpsc;

use crate::state::StateEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AppId(pub u64);

impl AppId {
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone)]
pub struct AppEvent {
    pub seq: u64,
    pub kind: AppEventKind,
}

#[derive(Debug, Clone)]
pub enum AppEventKind {
    Agent(AgentEventEnvelope),
    StateChanged(StateEvent),
    Notification { text: String },
    Error { message: String },
    Exited,
}

impl AppEvent {
    pub fn new(kind: AppEventKind) -> Self {
        Self { seq: 0, kind }
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

    pub fn state_changed(change: crate::state::StateChange) -> Self {
        Self::new(AppEventKind::StateChanged(StateEvent {
            revision: 0,
            change,
        }))
    }

    pub fn agent(event: AgentEvent) -> Self {
        Self::agent_with(AgentId(1), TurnId(1), event)
    }

    pub fn agent_with(agent_id: AgentId, turn_id: TurnId, event: AgentEvent) -> Self {
        Self::new(AppEventKind::Agent(AgentEventEnvelope {
            seq: 0,
            agent_id,
            turn_id,
            event,
        }))
    }
}

pub(crate) type Subscribers = Arc<Mutex<Vec<mpsc::UnboundedSender<AppEvent>>>>;

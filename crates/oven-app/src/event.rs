use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use oven_agent::{AgentEvent, AgentEventEnvelope, AgentId, TurnId};
use tokio::sync::mpsc;

use crate::state::{StateChange, StateEvent};

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
    Shell(ShellEvent),
    Notification { text: String },
    Error { message: String },
    Exited,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

    pub fn shell(event: ShellEvent) -> Self {
        Self::new(AppEventKind::Shell(event))
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

pub(crate) struct EventBus {
    subscribers: Subscribers,
    seq: u64,
    state_rev: u64,
}

impl EventBus {
    pub(crate) fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(Vec::new())),
            seq: 0,
            state_rev: 0,
        }
    }

    pub(crate) fn subscribers(&self) -> Subscribers {
        self.subscribers.clone()
    }

    pub(crate) fn emit(&mut self, kind: AppEventKind) {
        self.seq += 1;
        let event = AppEvent {
            seq: self.seq,
            kind,
        };
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
    }

    pub(crate) fn emit_state(&mut self, change: StateChange) {
        self.state_rev += 1;
        self.emit(AppEventKind::StateChanged(StateEvent {
            revision: self.state_rev,
            change,
        }));
    }

    pub(crate) fn emit_error(&mut self, message: impl Into<String>) {
        self.emit(AppEventKind::Error {
            message: message.into(),
        });
    }
}

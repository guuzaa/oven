use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use oven_agent::{AgentEvent, AgentEventEnvelope, AgentId, EventSink, TurnId, UserRequestId};
use tokio::sync::mpsc;

use crate::core::state::HistoryChangeReason;

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
    pub kind: AppEventKind,
}

#[derive(Debug, Clone)]
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
/// are, which is `AppState::subagents`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentEvent {
    /// Open a view on one subagent.
    Focus { id: AgentId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionEvent {
    Started,
    Completed {
        before_tokens: u32,
        after_tokens: u32,
    },
    Failed {
        error: String,
    },
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

    pub fn agent(event: AgentEvent) -> Self {
        Self::agent_with(AgentId(1), TurnId(1), event)
    }

    pub fn agent_with(agent_id: AgentId, turn_id: TurnId, event: AgentEvent) -> Self {
        Self::new(AppEventKind::Agent(AgentEventEnvelope {
            agent_id,
            turn_id,
            event,
        }))
    }
}

pub(crate) type Subscribers = Arc<Mutex<Vec<mpsc::UnboundedSender<AppEvent>>>>;

/// An agent's report, published straight to the frontend.
///
/// The conversation driver and every subagent use one. A subagent's events
/// reach the frontend while the runtime is busy driving a turn of its own,
/// or idle between turns, without the runtime forwarding them.
pub(crate) struct BusSink {
    bus: EventBus,
    agent_id: AgentId,
    turn_id: TurnId,
}

impl BusSink {
    pub(crate) fn new(bus: EventBus, agent_id: AgentId, turn_id: TurnId) -> Self {
        Self {
            bus,
            agent_id,
            turn_id,
        }
    }
}

impl EventSink for BusSink {
    fn emit(&mut self, event: AgentEvent) {
        self.bus.emit_agent(self.agent_id, self.turn_id, event);
    }
}

/// Every event a frontend can see, fanning out to each subscriber.
///
/// Cloneable and shareable on purpose: a turn's own sink emits into the bus
/// while the runtime keeps handing commands to the same one, and neither has
/// to borrow it exclusively to do so.
#[derive(Clone)]
pub(crate) struct EventBus {
    subscribers: Subscribers,
}

impl EventBus {
    pub(crate) fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn subscribers(&self) -> Subscribers {
        self.subscribers.clone()
    }

    pub(crate) fn emit(&self, kind: AppEventKind) {
        let event = AppEvent { kind };
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
    }

    /// Emits an agent event the runtime raises itself, rather than one it
    /// forwards from the agent's own event channel.
    pub(crate) fn emit_agent(&self, agent_id: AgentId, turn_id: TurnId, event: AgentEvent) {
        self.emit(AppEvent::agent_with(agent_id, turn_id, event).kind);
    }

    pub(crate) fn emit_error(&self, message: impl Into<String>) {
        let message = message.into();
        tracing::warn!(error = %message, "app error");
        self.emit(AppEventKind::Error { message });
    }
}

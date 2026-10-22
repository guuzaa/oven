//! The bus every event a frontend can see travels on.
//!
//! The event vocabulary itself belongs to `oven-proto`; what stays here is
//! the fan-out: one bus, many subscribers, and the sink a turn publishes
//! through.

use std::sync::{Arc, Mutex, PoisonError};

use oven_agent::{AgentEvent, AgentId, EventSink, TurnId};
use oven_proto::{AppEvent, AppEventKind};
use tokio::sync::mpsc;

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

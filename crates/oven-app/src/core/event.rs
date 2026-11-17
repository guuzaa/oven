//! The bus every event a frontend can see travels on.
//!
//! The event vocabulary itself belongs to `oven-proto`; what stays here is
//! the fan-out: one bus, many subscribers, and the sink a turn publishes
//! through.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use oven_proto::{
    AgentEvent, AgentEventEnvelope, AgentId, AppEvent, AppEventKind, EventSink, TurnId,
};
use tokio::sync::mpsc;

use crate::platform::trace::{self, TraceWriter};

/// The channels the bus fans out to.
///
/// The event is shared, not copied: a subscriber that reads a 64 KB tool
/// result or a stream of text deltas takes a handle, so the bus hands the
/// same event to every subscriber at the cost of one reference count.
pub(crate) type Subscribers = Arc<Mutex<Vec<mpsc::UnboundedSender<Arc<AppEvent>>>>>;

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
    /// Whether the run writes a trace of its events.
    trace: bool,
}

impl EventBus {
    pub(crate) fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(Vec::new())),
            trace: false,
        }
    }

    /// Whether the bus writes a trace of the run's events.
    ///
    /// The bus carries the switch: a run that was not asked to trace never
    /// starts a writer, so it leaves no file behind.
    pub(crate) fn with_trace(mut self, trace: bool) -> Self {
        self.trace = trace;
        self
    }

    pub(crate) fn subscribers(&self) -> Subscribers {
        self.subscribers.clone()
    }

    /// Records the agent's events into `path`, one per line, when the bus
    /// was asked to trace.
    ///
    /// The trace travels the same bus the frontend watches, so the file
    /// holds what the frontend saw, in the order it saw it.
    pub(crate) fn start_trace(&self, path: PathBuf) {
        if !self.trace {
            return;
        }
        let (sender, events) = mpsc::unbounded_channel();
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sender);
        tokio::spawn(TraceWriter::new(path, events).run());
    }

    pub(crate) fn emit(&self, kind: AppEventKind) {
        let event = Arc::new(AppEvent { kind });
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        subscribers.retain(|subscriber| subscriber.send(Arc::clone(&event)).is_ok());
    }

    /// Emits an agent event the runtime raises itself, rather than one it
    /// forwards from the agent's own event channel.
    ///
    /// Every agent event passes here: it is stamped with the moment it
    /// happened and projected onto the log before it reaches the
    /// subscribers.
    pub(crate) fn emit_agent(&self, agent_id: AgentId, turn_id: TurnId, event: AgentEvent) {
        let envelope = AgentEventEnvelope {
            agent_id,
            turn_id,
            at: oven_host::now_ms(),
            event,
        };
        trace::project(&envelope);
        self.emit(AppEvent::agent(envelope).kind);
    }

    pub(crate) fn emit_error(&self, message: impl Into<String>) {
        let message = message.into();
        tracing::warn!(error = %message, "app error");
        self.emit(AppEventKind::Error { message });
    }
}

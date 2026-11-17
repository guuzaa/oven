//! The bus every event a frontend can see travels on.
//!
//! The event vocabulary itself belongs to `oven-proto`; what stays here is
//! the fan-out: one bus, many subscribers, and the sink a turn publishes
//! through.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use oven_proto::{
    AgentEvent, AgentEventEnvelope, AgentId, AppEvent, AppEventKind, EventSink, TurnId,
};
use tokio::sync::mpsc;

use crate::platform::trace::{self, TraceSettings, TraceWriter};

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

    fn emit_at(&mut self, elapsed_ms: u64, event: AgentEvent) {
        self.bus
            .emit_agent_at(self.agent_id, self.turn_id, elapsed_ms, event);
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
    /// How much of the run its trace keeps, and how many traces stay.
    settings: TraceSettings,
}

impl EventBus {
    pub(crate) fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(Vec::new())),
            settings: TraceSettings::default(),
        }
    }

    /// How much of the run the trace keeps, and how many traces stay.
    ///
    /// The trace is written either way: this says how much of the run it
    /// holds.
    pub(crate) fn with_trace(mut self, settings: TraceSettings) -> Self {
        self.settings = settings;
        self
    }

    pub(crate) fn subscribers(&self) -> Subscribers {
        self.subscribers.clone()
    }

    /// Records the agent's events into a trace of their own, one per line.
    ///
    /// The trace travels the same bus the frontend watches, so the file
    /// holds what the frontend saw, in the order it saw it.
    pub(crate) fn start_trace(&self, root: &Path, session_id: Option<&str>) {
        let run = trace::start(session_id);
        let path = trace::trace_path(root, run.id());
        tracing::info!(path = %path.display(), "the run writes its trace");
        let (sender, events) = mpsc::unbounded_channel();
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sender);
        tokio::spawn(TraceWriter::new(root.to_path_buf(), run, events, self.settings).run());
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
        self.emit_agent_at(agent_id, turn_id, oven_host::monotonic_ms(), event);
    }

    /// Emits an agent event that happened at a known moment.
    ///
    /// An event the agent only reports once the call it belongs to is over
    /// keeps the moment it happened at, so the timeline it lands on says
    /// where the time went.
    pub(crate) fn emit_agent_at(
        &self,
        agent_id: AgentId,
        turn_id: TurnId,
        elapsed_ms: u64,
        event: AgentEvent,
    ) {
        let envelope = AgentEventEnvelope {
            agent_id,
            turn_id,
            at: wall_clock_at(elapsed_ms),
            elapsed_ms,
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

/// The wall-clock time of a moment on the monotonic clock.
fn wall_clock_at(elapsed_ms: u64) -> u64 {
    oven_host::now_ms().saturating_sub(oven_host::monotonic_ms().saturating_sub(elapsed_ms))
}

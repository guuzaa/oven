//! Where a turn reports what it did.

use crate::AgentEvent;

/// Where an agent puts what it did. The driver owns the sink for the length
/// of a turn; a frontend that also wants the events subscribes to the app
/// runtime's bus instead.
pub trait EventSink {
    fn emit(&mut self, event: AgentEvent);

    /// Emits an event that happened at a known moment.
    ///
    /// The moment is in the monotonic milliseconds the process counts from
    /// its start — the same clock the bus timestamps with. An event that
    /// happened inside a call, and is only reported once the call is over,
    /// keeps the moment it happened at.
    fn emit_at(&mut self, elapsed_ms: u64, event: AgentEvent) {
        let _ = elapsed_ms;
        self.emit(event);
    }
}

#[derive(Debug, Default)]
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&mut self, _event: AgentEvent) {}
}

#[derive(Debug, Default)]
pub struct VecEventSink {
    pub events: Vec<AgentEvent>,
}

impl EventSink for VecEventSink {
    fn emit(&mut self, event: AgentEvent) {
        self.events.push(event);
    }
}

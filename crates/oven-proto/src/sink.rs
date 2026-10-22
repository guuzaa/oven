//! Where a turn reports what it did.

use crate::AgentEvent;

/// Where an agent puts what it did. The driver owns the sink for the length
/// of a turn; a frontend that also wants the events subscribes to the app
/// runtime's bus instead.
pub trait EventSink {
    fn emit(&mut self, event: AgentEvent);
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

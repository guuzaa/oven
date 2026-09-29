use crate::event::AgentEvent;

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

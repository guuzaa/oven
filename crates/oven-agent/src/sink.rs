use tokio::sync::mpsc::UnboundedSender;

use crate::event::{AgentEvent, AgentEventEnvelope};
use crate::identity::{AgentId, TurnId};

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

pub struct ChannelEventSink {
    tx: UnboundedSender<AgentEventEnvelope>,
    seq: u64,
    agent_id: AgentId,
    turn_id: TurnId,
}

impl ChannelEventSink {
    pub fn new(
        tx: UnboundedSender<AgentEventEnvelope>,
        agent_id: AgentId,
        turn_id: TurnId,
    ) -> Self {
        Self {
            tx,
            seq: 0,
            agent_id,
            turn_id,
        }
    }
}

impl EventSink for ChannelEventSink {
    fn emit(&mut self, event: AgentEvent) {
        self.seq += 1;
        let _ = self.tx.send(AgentEventEnvelope {
            seq: self.seq,
            agent_id: self.agent_id,
            turn_id: self.turn_id,
            event,
        });
    }
}

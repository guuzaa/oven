use std::sync::{Arc, Mutex};

use crate::mode::AgentMode;

pub struct AgentLive {
    pub mode: AgentMode,
    pub base_system: Option<String>,
}

impl AgentLive {
    pub fn new(base_system: Option<String>) -> Self {
        Self {
            mode: AgentMode::Default,
            base_system,
        }
    }
}

pub type LiveHandle = Arc<Mutex<AgentLive>>;

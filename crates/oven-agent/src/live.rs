use std::sync::{Arc, Mutex};

use crate::mode::AgentMode;
use crate::todo::TodoList;

pub struct AgentLive {
    pub mode: AgentMode,
    pub base_system: Option<String>,
    pub todos: TodoList,
}

impl AgentLive {
    pub fn new(base_system: Option<String>) -> Self {
        Self {
            mode: AgentMode::Default,
            base_system,
            todos: TodoList::default(),
        }
    }
}

pub type LiveHandle = Arc<Mutex<AgentLive>>;

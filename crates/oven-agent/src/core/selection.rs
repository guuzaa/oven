use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use oven_llm::{ModelId, ReasoningEffort};

use crate::core::mode::AgentMode;

pub type ModelSelection = (ModelId, Option<ReasoningEffort>);

#[derive(Debug)]
struct Selected {
    mode: AgentMode,
    model: ModelId,
    reasoning_effort: Option<ReasoningEffort>,
}

/// The mode and model an agent's next step runs with.
///
/// Cloned handles share one value, so a caller can change it while a turn
/// holds `&mut Agent`: the turn reads it again at each step and picks the
/// change up there instead of after the whole turn.
#[derive(Debug, Clone)]
pub struct Selection(Arc<Mutex<Selected>>);

impl Selection {
    pub fn new(mode: AgentMode, model: ModelId, reasoning_effort: Option<ReasoningEffort>) -> Self {
        Self(Arc::new(Mutex::new(Selected {
            mode,
            model,
            reasoning_effort,
        })))
    }

    fn lock(&self) -> MutexGuard<'_, Selected> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn mode(&self) -> AgentMode {
        self.lock().mode
    }

    pub fn set_mode(&self, mode: AgentMode) {
        self.lock().mode = mode;
    }

    pub fn model(&self) -> ModelSelection {
        let selected = self.lock();
        (selected.model.clone(), selected.reasoning_effort)
    }

    pub fn set_model(&self, model: ModelId) {
        self.lock().model = model;
    }

    pub fn set_reasoning_effort(&self, reasoning_effort: Option<ReasoningEffort>) {
        self.lock().reasoning_effort = reasoning_effort;
    }

    pub fn switch_model(&self, model: ModelId, reasoning_effort: Option<ReasoningEffort>) {
        let mut selected = self.lock();
        selected.model = model;
        selected.reasoning_effort = reasoning_effort;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_one_selection() {
        let selection = Selection::new(AgentMode::Agent, ModelId::new("a"), None);
        let other = selection.clone();
        other.set_mode(AgentMode::Plan);
        other.switch_model(ModelId::new("b"), Some(ReasoningEffort::Low));
        assert_eq!(selection.mode(), AgentMode::Plan);
        assert_eq!(
            selection.model(),
            (ModelId::new("b"), Some(ReasoningEffort::Low))
        );
    }
}

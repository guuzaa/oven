#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentMode {
    #[default]
    Default,
    Plan,
}

impl AgentMode {
    pub fn toggle(self) -> Self {
        match self {
            Self::Default => Self::Plan,
            Self::Plan => Self::Default,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "agent",
            Self::Plan => "plan",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_default() {
        assert_eq!(AgentMode::default(), AgentMode::Default);
    }

    #[test]
    fn toggle_swaps_default_and_plan() {
        assert_eq!(AgentMode::Default.toggle(), AgentMode::Plan);
        assert_eq!(AgentMode::Plan.toggle(), AgentMode::Default);
    }

    #[test]
    fn label_matches_mode() {
        assert_eq!(AgentMode::Default.label(), "agent");
        assert_eq!(AgentMode::Plan.label(), "plan");
    }
}

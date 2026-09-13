use crate::tools::ToolPermission;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAccess {
    Hidden,
    Allowed,
    RequiresApproval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentMode {
    #[default]
    Agent,
    Plan,
    Ask,
}

impl AgentMode {
    pub fn toggle(self) -> Self {
        match self {
            Self::Agent => Self::Plan,
            Self::Plan => Self::Ask,
            Self::Ask => Self::Agent,
        }
    }

    pub fn tool_access(self, permission: ToolPermission) -> ToolAccess {
        match self {
            Self::Agent | Self::Plan => ToolAccess::Allowed,
            Self::Ask => match permission {
                ToolPermission::Read => ToolAccess::Allowed,
                ToolPermission::Execute => ToolAccess::RequiresApproval,
                ToolPermission::Write | ToolPermission::External => ToolAccess::Hidden,
            },
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Plan => "plan",
            Self::Ask => "ask",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_default() {
        assert_eq!(AgentMode::default(), AgentMode::Agent);
    }

    #[test]
    fn toggle_cycles_all_modes() {
        assert_eq!(AgentMode::Agent.toggle(), AgentMode::Plan);
        assert_eq!(AgentMode::Plan.toggle(), AgentMode::Ask);
        assert_eq!(AgentMode::Ask.toggle(), AgentMode::Agent);
    }

    #[test]
    fn label_matches_mode() {
        assert_eq!(AgentMode::Agent.label(), "agent");
        assert_eq!(AgentMode::Plan.label(), "plan");
        assert_eq!(AgentMode::Ask.label(), "ask");
    }
}

use crate::core::mode::AgentMode;

pub const PLAN_MODE_PROMPT: &str = include_str!("plan.md");
pub const ASK_MODE_PROMPT: &str = include_str!("ask.md");

pub const PLAN_REMINDER: &str = "\
<reminder>
Five tool rounds have run without calling todo_write.
Update the list now if any item's status changed. At most one item may be in_progress.
</reminder>";

pub fn compose_system(base: Option<&str>, mode: AgentMode) -> Option<String> {
    match (base, mode) {
        (None, AgentMode::Agent) => None,
        (None, AgentMode::Plan) => Some(PLAN_MODE_PROMPT.to_string()),
        (None, AgentMode::Ask) => Some(ASK_MODE_PROMPT.to_string()),
        (Some(base), AgentMode::Agent) => Some(base.to_string()),
        (Some(base), AgentMode::Plan) => Some(format!("{base}\n\n{PLAN_MODE_PROMPT}")),
        (Some(base), AgentMode::Ask) => Some(format!("{base}\n\n{ASK_MODE_PROMPT}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_system_default_vs_plan() {
        assert_eq!(compose_system(None, AgentMode::Agent), None);
        assert_eq!(
            compose_system(None, AgentMode::Plan).as_deref(),
            Some(PLAN_MODE_PROMPT)
        );
        assert_eq!(
            compose_system(Some("base"), AgentMode::Agent).as_deref(),
            Some("base")
        );
        assert_eq!(
            compose_system(Some("base"), AgentMode::Plan),
            Some(format!("base\n\n{PLAN_MODE_PROMPT}"))
        );
    }

    #[test]
    fn mode_system_omits_the_checklist() {
        let out = compose_system(Some("base"), AgentMode::Plan).unwrap();
        assert!(!out.contains("## Current TODO list"));
        assert!(!out.contains("<reminder>"));
    }

    #[test]
    fn plan_reminder_is_wrapped_for_a_user_message() {
        assert!(PLAN_REMINDER.starts_with("<reminder>\n"));
        assert!(PLAN_REMINDER.ends_with("\n</reminder>"));
        assert!(PLAN_REMINDER.contains("todo_write"));
    }
}

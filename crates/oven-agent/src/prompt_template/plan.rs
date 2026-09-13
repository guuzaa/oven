use crate::mode::AgentMode;
use crate::todo::TodoList;

pub const PLAN_MODE_PROMPT: &str = include_str!("plan.md");
pub const ASK_MODE_PROMPT: &str = include_str!("ask.md");

pub const PLAN_REMINDER: &str = "\
## Plan reminder
The previous step used tools but did not call todo_write.
Update the list now if any item's status changed. At most one item may be in_progress.";

pub fn compose_todo_system(
    base: Option<&str>,
    mode: AgentMode,
    todos: &TodoList,
    remind: bool,
) -> Option<String> {
    let mut system = match (base, mode) {
        (None, AgentMode::Agent) => None,
        (None, AgentMode::Plan) => Some(PLAN_MODE_PROMPT.to_string()),
        (None, AgentMode::Ask) => Some(ASK_MODE_PROMPT.to_string()),
        (Some(base), AgentMode::Agent) => Some(base.to_string()),
        (Some(base), AgentMode::Plan) => Some(format!("{base}\n\n{PLAN_MODE_PROMPT}")),
        (Some(base), AgentMode::Ask) => Some(format!("{base}\n\n{ASK_MODE_PROMPT}")),
    };
    if !todos.is_empty() {
        let block = todos.render_todo_block();
        match system.as_mut() {
            Some(s) => {
                s.push_str("\n\n");
                s.push_str(&block);
            }
            None => system = Some(block),
        }
    }
    if remind && let Some(s) = system.as_mut() {
        s.push_str("\n\n");
        s.push_str(PLAN_REMINDER);
    }
    system
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo::{TodoItem, TodoStatus};

    fn item(id: &str, content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            id: id.into(),
            content: content.into(),
            status,
        }
    }

    #[test]
    fn compose_system_default_vs_plan() {
        assert_eq!(
            compose_todo_system(None, AgentMode::Agent, &TodoList::default(), false),
            None
        );
        assert_eq!(
            compose_todo_system(None, AgentMode::Plan, &TodoList::default(), false).as_deref(),
            Some(PLAN_MODE_PROMPT)
        );
        assert_eq!(
            compose_todo_system(Some("base"), AgentMode::Agent, &TodoList::default(), false)
                .as_deref(),
            Some("base")
        );
        assert_eq!(
            compose_todo_system(Some("base"), AgentMode::Plan, &TodoList::default(), false),
            Some(format!("base\n\n{PLAN_MODE_PROMPT}"))
        );
    }

    #[test]
    fn render_todo_block_lists_status_id_and_content() {
        let list = TodoList {
            items: vec![
                item("impl-mode", "Add AgentMode", TodoStatus::InProgress),
                item("tui-toggle", "Handle BackTab", TodoStatus::Pending),
            ],
        };
        let block = list.render_todo_block();
        assert!(block.starts_with("## Current TODO list\n"));
        assert!(block.contains("- [in_progress] `impl-mode` Add AgentMode"));
        assert!(block.contains("- [pending] `tui-toggle` Handle BackTab"));
    }

    #[test]
    fn empty_list_adds_no_todo_block() {
        let out = compose_todo_system(Some("base"), AgentMode::Agent, &TodoList::default(), false)
            .unwrap();
        assert_eq!(out, "base");
        assert!(!out.contains("## Current TODO list"));
    }

    #[test]
    fn compose_appends_todos_and_reminder() {
        let list = TodoList {
            items: vec![item("a", "one", TodoStatus::Pending)],
        };
        let out = compose_todo_system(Some("base"), AgentMode::Plan, &list, true).unwrap();
        assert!(out.contains("base"));
        assert!(out.contains("# Plan Mode"));
        assert!(out.contains("## Current TODO list"));
        assert!(out.contains("- [pending] `a` one"));
        assert!(out.contains("## Plan reminder"));
        assert!(out.find("# Plan Mode").unwrap() < out.find("## Current TODO list").unwrap());
        assert!(out.find("## Current TODO list").unwrap() < out.find("## Plan reminder").unwrap());
    }

    #[test]
    fn reminder_not_injected_when_system_would_be_empty() {
        assert_eq!(
            compose_todo_system(None, AgentMode::Agent, &TodoList::default(), true),
            None
        );
    }
}

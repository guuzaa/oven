use serde_json::Value;

use super::{BashTool, FileEditTool, FileReadTool, FileWriteTool, TodoWriteTool};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolView {
    pub summary: String,
    pub collapse: bool,
    pub diff: bool,
}

impl ToolView {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            summary: name.into(),
            collapse: true,
            diff: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ToolCaps {
    pub plan_only: bool,
    pub writes_todos: bool,
}

pub fn present_tool(name: &str, input: &Value) -> ToolView {
    match name {
        BashTool::NAME => BashTool::view_input(input),
        FileReadTool::NAME => FileReadTool::view_input(input),
        FileEditTool::NAME => FileEditTool::view_input(input),
        FileWriteTool::NAME => FileWriteTool::view_input(input),
        TodoWriteTool::NAME => TodoWriteTool::view_input(input),
        _ => ToolView::named(name),
    }
}

fn input_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub(crate) fn labeled(name: &str, verb: &str, input: &Value, key: &str) -> ToolView {
    match input_str(input, key) {
        Some(v) => ToolView {
            summary: format!("{verb} {v}"),
            collapse: true,
            diff: false,
        },
        None => ToolView::named(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn present_tool_uses_command_and_path() {
        assert_eq!(
            present_tool(BashTool::NAME, &json!({ "command": "ls -la" })).summary,
            "Ran ls -la"
        );
        assert_eq!(
            present_tool(FileReadTool::NAME, &json!({ "path": "src/main.rs" })).summary,
            "Read src/main.rs"
        );
        assert_eq!(
            present_tool(
                FileEditTool::NAME,
                &json!({
                    "path": "src/main.rs",
                    "old_string": "old",
                    "new_string": "new"
                })
            )
            .summary,
            "Edit src/main.rs\n- old\n+ new"
        );
        assert!(
            !present_tool(
                FileEditTool::NAME,
                &json!({
                    "path": "src/main.rs",
                    "old_string": "old",
                    "new_string": "new"
                })
            )
            .collapse
        );
        assert_eq!(
            present_tool(
                FileWriteTool::NAME,
                &json!({ "path": "out.txt", "content": "new content" })
            )
            .summary,
            "Write out.txt\n+ new content"
        );
        assert!(
            !present_tool(
                FileWriteTool::NAME,
                &json!({ "path": "out.txt", "content": "new content" })
            )
            .collapse
        );
        assert_eq!(
            present_tool("grep", &json!({ "pattern": "foo" })).summary,
            "grep"
        );
        assert_eq!(
            present_tool(BashTool::NAME, &json!({})).summary,
            BashTool::NAME
        );
        let todo = present_tool(
            TodoWriteTool::NAME,
            &json!({ "todos": [{"id": "a", "content": "one", "status": "pending"}] }),
        );
        assert!(!todo.collapse);
        assert_eq!(
            todo.summary,
            "todo_write · 1 todos (0 in_progress, 0 completed)"
        );
        assert!(present_tool(TodoWriteTool::NAME, &json!({})).summary == TodoWriteTool::NAME);
        assert!(!present_tool(TodoWriteTool::NAME, &json!({})).collapse);
    }
}

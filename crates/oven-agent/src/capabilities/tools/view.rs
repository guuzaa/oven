//! How one built-in tool renders a call for a frontend.
//!
//! [`present_tool`] is the table a driver consults once per `ToolEvent`; the
//! nouns it returns live in [`crate::core::view`].

use serde_json::Value;

use crate::core::view::ToolView;

use super::{
    AnswerTool, BashTool, FileEditTool, FileReadTool, FileWriteTool, GlobTool, GrepTool,
    TaskOutputTool, TaskTool, TodoWriteTool, WebFetchTool,
};

pub fn present_tool(name: &str, input: &Value) -> ToolView {
    match name {
        BashTool::NAME => BashTool::view_input(input),
        FileReadTool::NAME => FileReadTool::view_input(input),
        FileEditTool::NAME => FileEditTool::view_input(input),
        FileWriteTool::NAME => FileWriteTool::view_input(input),
        GlobTool::NAME => GlobTool::view_input(input),
        GrepTool::NAME => GrepTool::view_input(input),
        WebFetchTool::NAME => WebFetchTool::view_input(input),
        TodoWriteTool::NAME => TodoWriteTool::view_input(input),
        TaskTool::NAME => TaskTool::view_input(input),
        TaskOutputTool::NAME => TaskOutputTool::view_input(input),
        AnswerTool::NAME => AnswerTool::view_input(input),
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
            detail: None,
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
        let edit = present_tool(
            FileEditTool::NAME,
            &json!({
                "path": "src/main.rs",
                "old_string": "old",
                "new_string": "new"
            }),
        );
        assert_eq!(edit.summary, "Edit src/main.rs");
        assert_eq!(edit.detail.as_deref(), Some("- old\n+ new"));
        assert!(edit.collapse);
        assert_eq!(
            present_tool(
                FileWriteTool::NAME,
                &json!({ "path": "out.txt", "content": "new content" })
            )
            .summary,
            "Write out.txt"
        );
        assert_eq!(
            present_tool(
                FileWriteTool::NAME,
                &json!({ "path": "out.txt", "content": "new content" })
            )
            .detail
            .as_deref(),
            Some("+ new content")
        );
        assert!(
            present_tool(
                FileWriteTool::NAME,
                &json!({ "path": "out.txt", "content": "new content" })
            )
            .collapse
        );
        assert_eq!(
            present_tool(GrepTool::NAME, &json!({ "pattern": "foo" })).summary,
            "Search foo"
        );
        assert_eq!(
            present_tool(
                WebFetchTool::NAME,
                &json!({ "url": "https://example.com/docs" })
            )
            .summary,
            "Fetch https://example.com/docs"
        );
        assert_eq!(
            present_tool(WebFetchTool::NAME, &json!({})).summary,
            WebFetchTool::NAME
        );
        assert_eq!(
            present_tool(
                GlobTool::NAME,
                &json!({ "pattern": "**/*.rs", "path": "src" })
            )
            .summary,
            "Find **/*.rs in src"
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
        assert_eq!(todo.detail, None);
        assert_eq!(
            todo.summary,
            "todo_write · 1 todos (0 in_progress, 0 completed)"
        );
        assert!(present_tool(TodoWriteTool::NAME, &json!({})).summary == TodoWriteTool::NAME);
        assert!(!present_tool(TodoWriteTool::NAME, &json!({})).collapse);
        assert_eq!(
            present_tool(
                TaskTool::NAME,
                &json!({ "description": "find it", "role": "explore" })
            )
            .summary,
            "Agent explore: find it"
        );
        assert_eq!(
            present_tool(TaskOutputTool::NAME, &json!({ "name": "explore#1" })).summary,
            "Agent status: explore#1"
        );
        assert_eq!(
            present_tool(
                AnswerTool::NAME,
                &json!({ "question": "which database?", "options": [{ "label": "postgres" }] })
            )
            .summary,
            "Ask which database?"
        );
        assert_eq!(
            present_tool(AnswerTool::NAME, &json!({})).summary,
            AnswerTool::NAME
        );
    }
}

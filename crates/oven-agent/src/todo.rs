use oven_llm::{ContentBlock, Message};
use serde::{Deserialize, Serialize};

use crate::history::Record;
use crate::mode::AgentMode;

pub const PLAN_MODE_PROMPT: &str = "\
## Plan Mode

You are in Plan mode. Track multi-step work with the `todo_write` tool.

Rules:
- Before doing multi-step work, call `todo_write` with the full task list as JSON.
- At most one item may be `in_progress` at a time (zero is allowed when the
  list is empty or every item is completed/cancelled).
- Mark an item `in_progress` before you start it.
- After a step succeeds or is abandoned, call `todo_write` again with the
  complete updated list (`completed` or `cancelled`).
- Do not rewrite ids. Update `status` (and `content` only if the task itself changed).
- Keep the list short and actionable (prefer ≤ 12 items). Split later if needed.
- When the list is empty and the user asks for a simple one-shot, answer
  normally without creating a TODO list.
- The current list (if any) is appended below by the system; treat it as source of truth.";

pub const PLAN_REMINDER: &str = "\
## Plan reminder
The previous step used tools but did not call todo_write.
Update the list now if any item's status changed. At most one item may be in_progress.";

pub fn compose_system(base: Option<&str>, mode: AgentMode) -> Option<String> {
    match (base, mode) {
        (None, AgentMode::Default) => None,
        (None, AgentMode::Plan) => Some(PLAN_MODE_PROMPT.to_string()),
        (Some(base), AgentMode::Default) => Some(base.to_string()),
        (Some(base), AgentMode::Plan) => Some(format!("{base}\n\n{PLAN_MODE_PROMPT}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoList {
    pub items: Vec<TodoItem>,
}

impl TodoList {
    pub const MAX_ITEMS: usize = 40;
    pub const MAX_CONTENT: usize = 200;
    pub const MAX_ID: usize = 64;

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        let todos = value
            .get("todos")
            .ok_or_else(|| "todo_write: missing 'todos' array".to_string())?;
        let arr = todos
            .as_array()
            .ok_or_else(|| "todo_write: 'todos' must be an array".to_string())?;
        if arr.len() > Self::MAX_ITEMS {
            return Err(format!(
                "todo_write: too many items (max {})",
                Self::MAX_ITEMS
            ));
        }
        let mut items = Vec::with_capacity(arr.len());
        let mut seen = std::collections::HashSet::new();
        let mut in_progress = 0usize;
        for (i, v) in arr.iter().enumerate() {
            let id = v
                .get("id")
                .and_then(|x| x.as_str())
                .ok_or_else(|| format!("todo_write: item {i}: missing 'id' string"))?;
            if id.is_empty() {
                return Err("todo_write: empty id".into());
            }
            if id.chars().count() > Self::MAX_ID {
                return Err(format!("todo_write: id too long (max {})", Self::MAX_ID));
            }
            if !seen.insert(id) {
                return Err(format!("todo_write: duplicate id '{id}'"));
            }
            let content = v
                .get("content")
                .and_then(|x| x.as_str())
                .ok_or_else(|| format!("todo_write: item {i}: missing 'content' string"))?;
            if content.is_empty() {
                return Err("todo_write: empty content".into());
            }
            if content.chars().count() > Self::MAX_CONTENT {
                return Err(format!(
                    "todo_write: content too long (max {})",
                    Self::MAX_CONTENT
                ));
            }
            let status = match v.get("status") {
                Some(s) => serde_json::from_value::<TodoStatus>(s.clone())
                    .map_err(|_| "todo_write: invalid status".to_string())?,
                None => return Err("todo_write: missing status".into()),
            };
            if status == TodoStatus::InProgress {
                in_progress += 1;
            }
            items.push(TodoItem {
                id: id.to_string(),
                content: content.to_string(),
                status,
            });
        }
        if in_progress > 1 {
            return Err("todo_write: more than one in_progress item".into());
        }
        Ok(Self { items })
    }

    pub fn summary(&self) -> String {
        let n = self.items.len();
        let in_progress = self
            .items
            .iter()
            .filter(|i| i.status == TodoStatus::InProgress)
            .count();
        let completed = self
            .items
            .iter()
            .filter(|i| i.status == TodoStatus::Completed)
            .count();
        format!("{n} todos ({in_progress} in_progress, {completed} completed)")
    }

    pub fn render_prompt_block(&self) -> String {
        if self.items.is_empty() {
            return String::new();
        }
        let mut out = String::from("## Current TODO list\n");
        for item in &self.items {
            let status = match item.status {
                TodoStatus::Pending => "pending",
                TodoStatus::InProgress => "in_progress",
                TodoStatus::Completed => "completed",
                TodoStatus::Cancelled => "cancelled",
            };
            out.push_str(&format!("- [{status}] `{}` {}\n", item.id, item.content));
        }
        out
    }

    pub fn from_history<'a>(messages: impl Iterator<Item = &'a Message>) -> Option<Self> {
        let messages: Vec<_> = messages.collect();
        for m in messages.into_iter().rev() {
            for block in m.content.iter().rev() {
                if let ContentBlock::ToolUse { input, .. } = block
                    && let Ok(list) = Self::parse(input)
                {
                    return Some(list);
                }
            }
        }
        None
    }
}

pub fn restore_todos<'a>(
    records: &[Record],
    messages: impl Iterator<Item = &'a Message>,
) -> TodoList {
    if let Some(items) = records.iter().rev().find_map(|r| match r {
        Record::TodoList { items, .. } => Some(items.clone()),
        _ => None,
    }) {
        return TodoList { items };
    }
    TodoList::from_history(messages).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oven_llm::Role;
    use serde_json::json;

    fn item(id: &str, content: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            id: id.into(),
            content: content.into(),
            status,
        }
    }

    fn todo_use(id: &str, input: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: "todo_write".into(),
                input,
            }],
        }
    }

    #[test]
    fn parse_success() {
        let list = TodoList::parse(&json!({
            "todos": [
                {"id": "a", "content": "one", "status": "pending"},
                {"id": "b", "content": "two", "status": "in_progress"},
                {"id": "c", "content": "three", "status": "completed"}
            ]
        }))
        .unwrap();
        assert_eq!(list.items.len(), 3);
        assert_eq!(list.items[1].status, TodoStatus::InProgress);
        assert_eq!(list.summary(), "3 todos (1 in_progress, 1 completed)");
    }

    #[test]
    fn parse_missing_todos() {
        let err = TodoList::parse(&json!({"items": []})).unwrap_err();
        assert!(err.contains("todo_write: missing 'todos' array"));
    }

    #[test]
    fn parse_todos_not_array() {
        let err = TodoList::parse(&json!({"todos": "nope"})).unwrap_err();
        assert!(err.contains("must be an array"));
    }

    #[test]
    fn parse_duplicate_id() {
        let err = TodoList::parse(&json!({
            "todos": [
                {"id": "x", "content": "one", "status": "pending"},
                {"id": "x", "content": "two", "status": "completed"}
            ]
        }))
        .unwrap_err();
        assert_eq!(err, "todo_write: duplicate id 'x'");
    }

    #[test]
    fn parse_two_in_progress() {
        let err = TodoList::parse(&json!({
            "todos": [
                {"id": "a", "content": "one", "status": "in_progress"},
                {"id": "b", "content": "two", "status": "in_progress"}
            ]
        }))
        .unwrap_err();
        assert!(err.contains("more than one in_progress"));
    }

    #[test]
    fn parse_content_too_long() {
        let err = TodoList::parse(&json!({
            "todos": [{
                "id": "a",
                "content": "x".repeat(TodoList::MAX_CONTENT + 1),
                "status": "pending"
            }]
        }))
        .unwrap_err();
        assert!(err.contains("content too long"));
    }

    #[test]
    fn parse_limits_use_unicode_scalar_count() {
        let content: String = "你".repeat(TodoList::MAX_CONTENT);
        let list = TodoList::parse(&json!({
            "todos": [{"id": "a", "content": content, "status": "pending"}]
        }))
        .unwrap();
        assert_eq!(list.items[0].content.chars().count(), TodoList::MAX_CONTENT);

        let err = TodoList::parse(&json!({
            "todos": [{
                "id": "a",
                "content": "你".repeat(TodoList::MAX_CONTENT + 1),
                "status": "pending"
            }]
        }))
        .unwrap_err();
        assert!(err.contains("content too long"));

        let id: String = "项".repeat(TodoList::MAX_ID);
        TodoList::parse(&json!({
            "todos": [{"id": id, "content": "ok", "status": "pending"}]
        }))
        .unwrap();

        let err = TodoList::parse(&json!({
            "todos": [{
                "id": "项".repeat(TodoList::MAX_ID + 1),
                "content": "ok",
                "status": "pending"
            }]
        }))
        .unwrap_err();
        assert!(err.contains("id too long"));
    }

    #[test]
    fn parse_empty_list_allowed() {
        let list = TodoList::parse(&json!({"todos": []})).unwrap();
        assert!(list.is_empty());
        assert_eq!(list.summary(), "0 todos (0 in_progress, 0 completed)");
        assert!(list.render_prompt_block().is_empty());
    }

    #[test]
    fn render_prompt_block_lists_status_id_and_content() {
        let list = TodoList {
            items: vec![
                item("impl-mode", "Add AgentMode", TodoStatus::InProgress),
                item("tui-toggle", "Handle BackTab", TodoStatus::Pending),
            ],
        };
        let block = list.render_prompt_block();
        assert!(block.starts_with("## Current TODO list\n"));
        assert!(block.contains("- [in_progress] `impl-mode` Add AgentMode"));
        assert!(block.contains("- [pending] `tui-toggle` Handle BackTab"));
    }

    #[test]
    fn from_history_uses_last_parseable_write() {
        let first = todo_use(
            "c1",
            json!({"todos":[{"id":"a","content":"one","status":"pending"}]}),
        );
        let bad = todo_use("c2", json!({"todos": "nope"}));
        let last = todo_use(
            "c3",
            json!({"todos":[{"id":"a","content":"one","status":"completed"}]}),
        );
        let messages = [
            Message::user_text("go"),
            first,
            bad,
            last,
            Message::assistant(vec![ContentBlock::text("done")]),
        ];
        let list = TodoList::from_history(messages.iter()).unwrap();
        assert_eq!(list.items[0].status, TodoStatus::Completed);
    }

    #[test]
    fn from_history_none_when_never_written() {
        let messages = [
            Message::user_text("hi"),
            Message::assistant(vec![ContentBlock::text("hello")]),
        ];
        assert!(TodoList::from_history(messages.iter()).is_none());
    }

    #[test]
    fn from_history_empty_write_is_some() {
        let messages = [todo_use("c1", json!({"todos": []}))];
        let list = TodoList::from_history(messages.iter()).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn restore_todos_prefers_last_todo_list_including_empty() {
        let write = todo_use(
            "c1",
            json!({"todos":[{"id":"a","content":"one","status":"pending"}]}),
        );
        let records = vec![
            Record::Message {
                timestamp: 1,
                message: write.clone(),
            },
            Record::TodoList {
                timestamp: 2,
                items: vec![],
            },
        ];
        let restored = restore_todos(&records, std::iter::once(&write));
        assert!(restored.is_empty());
    }

    #[test]
    fn restore_todos_falls_back_to_from_history() {
        let write = todo_use(
            "c1",
            json!({"todos":[{"id":"a","content":"one","status":"in_progress"}]}),
        );
        let records = vec![Record::Message {
            timestamp: 1,
            message: write.clone(),
        }];
        let restored = restore_todos(&records, std::iter::once(&write));
        assert_eq!(restored.items[0].status, TodoStatus::InProgress);
    }

    #[test]
    fn compose_system_default_vs_plan() {
        assert_eq!(compose_system(None, AgentMode::Default), None);
        assert_eq!(
            compose_system(None, AgentMode::Plan).as_deref(),
            Some(PLAN_MODE_PROMPT)
        );
        assert_eq!(
            compose_system(Some("base"), AgentMode::Default).as_deref(),
            Some("base")
        );
        assert_eq!(
            compose_system(Some("base"), AgentMode::Plan),
            Some(format!("base\n\n{PLAN_MODE_PROMPT}"))
        );
    }
}

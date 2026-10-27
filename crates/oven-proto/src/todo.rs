//! The checklist a turn keeps.

use std::collections::HashSet;
use std::fmt::Write;

use oven_llm::{ContentBlock, Message};
use serde::{Deserialize, Serialize};

use crate::Record;

/// The tool that writes the checklist. Named here because the checklist is
/// restored from the history by looking for this call.
pub const TODO_WRITE_TOOL: &str = "todo_write";

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

#[derive(Deserialize)]
struct TodoWriteArgs {
    todos: Vec<TodoItem>,
}

impl TodoList {
    pub const MAX_ITEMS: usize = 40;
    pub const MAX_CONTENT: usize = 200;
    pub const MAX_ID: usize = 64;

    /// The checklist a reloaded session starts from: the last one written
    /// into its records, or the last one the history wrote.
    pub fn restore<'a>(records: &[Record], messages: impl Iterator<Item = &'a Message>) -> Self {
        if let Some(items) = records.iter().rev().find_map(|r| match r {
            Record::TodoList { items, .. } => Some(items.clone()),
            _ => None,
        }) {
            return TodoList { items };
        }
        Self::from_history(messages).unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Non-empty and every item is completed or cancelled.
    pub fn is_finished(&self) -> bool {
        !self.items.is_empty()
            && self
                .items
                .iter()
                .all(|item| matches!(item.status, TodoStatus::Completed | TodoStatus::Cancelled))
    }

    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        let args = TodoWriteArgs::deserialize(value).map_err(|e| format!("todo_write: {e}"))?;
        Self::validate(args.todos)
    }

    fn validate(items: Vec<TodoItem>) -> Result<Self, String> {
        if items.len() > Self::MAX_ITEMS {
            return Err(format!(
                "todo_write: too many items (max {})",
                Self::MAX_ITEMS
            ));
        }
        let mut seen = HashSet::new();
        let mut in_progress = 0usize;
        for item in &items {
            if item.id.is_empty() {
                return Err("todo_write: empty id".into());
            }
            if item.id.chars().count() > Self::MAX_ID {
                return Err(format!("todo_write: id too long (max {})", Self::MAX_ID));
            }
            if !seen.insert(item.id.as_str()) {
                return Err(format!("todo_write: duplicate id '{}'", item.id));
            }
            if item.content.is_empty() {
                return Err("todo_write: empty content".into());
            }
            if item.content.chars().count() > Self::MAX_CONTENT {
                return Err(format!(
                    "todo_write: content too long (max {})",
                    Self::MAX_CONTENT
                ));
            }
            if item.status == TodoStatus::InProgress {
                in_progress += 1;
            }
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

    pub fn from_history<'a>(messages: impl Iterator<Item = &'a Message>) -> Option<Self> {
        let messages: Vec<_> = messages.collect();
        messages
            .into_iter()
            .rev()
            .flat_map(|m| m.content.iter().rev())
            .find_map(|block| match block {
                ContentBlock::ToolUse(call) if call.name() == TODO_WRITE_TOOL => {
                    Self::parse(call.input()).ok()
                }
                _ => None,
            })
    }

    pub fn render_todo_block(&self) -> String {
        let mut out = String::from("## Current TODO list\n");
        for item in &self.items {
            let status = match item.status {
                TodoStatus::Pending => "pending",
                TodoStatus::InProgress => "in_progress",
                TodoStatus::Completed => "completed",
                TodoStatus::Cancelled => "cancelled",
            };
            let _ = writeln!(out, "- [{status}] `{}` {}", item.id, item.content);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oven_llm::{Role, ToolUse};
    use serde_json::json;

    fn todo_use(id: &str, input: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse(ToolUse::new(
                id,
                TODO_WRITE_TOOL,
                input,
            ))],
        }
    }

    #[test]
    fn the_list_survives_the_wire() {
        let list = TodoList {
            items: vec![TodoItem {
                id: "a".into(),
                content: "one".into(),
                status: TodoStatus::InProgress,
            }],
        };
        let json = serde_json::to_string(&list).unwrap();
        assert_eq!(serde_json::from_str::<TodoList>(&json).unwrap(), list);
    }

    #[test]
    fn parse_rejects_two_in_progress_items() {
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
    fn from_history_uses_last_parseable_write() {
        let first = todo_use(
            "c1",
            json!({"todos":[{"id":"a","content":"one","status":"pending"}]}),
        );
        let last = todo_use(
            "c3",
            json!({"todos":[{"id":"a","content":"one","status":"completed"}]}),
        );
        let messages = [first, last];
        let list = TodoList::from_history(messages.iter()).unwrap();
        assert_eq!(list.items[0].status, TodoStatus::Completed);
    }
}

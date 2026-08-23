use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolCaps, ToolView};
use crate::error::AgentError;
use crate::todo::TodoList;

pub struct TodoWriteTool;

impl TodoWriteTool {
    pub const NAME: &'static str = "todo_write";

    pub fn view_input(input: &Value) -> ToolView {
        let summary = match TodoList::parse(input) {
            Ok(list) => format!("{} · {}", Self::NAME, list.summary()),
            Err(_) => Self::NAME.to_string(),
        };
        ToolView {
            summary,
            collapse: false,
        }
    }
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn view(&self, input: &Value) -> ToolView {
        Self::view_input(input)
    }

    fn caps(&self) -> ToolCaps {
        ToolCaps {
            plan_only: true,
            writes_todos: true,
        }
    }

    fn description(&self) -> &str {
        "Replace the session TODO list with the given JSON array. Always send the\n\
         complete list (full replace, not a patch). Use short stable ids. At most\n\
         one item may be in_progress. Call this before starting multi-step work and\n\
         again after each step's status changes."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["todos"],
            "additionalProperties": false,
            "properties": {
                "todos": {
                    "type": "array",
                    "maxItems": 40,
                    "items": {
                        "type": "object",
                        "required": ["id", "content", "status"],
                        "additionalProperties": false,
                        "properties": {
                            "id": { "type": "string", "minLength": 1, "maxLength": 64 },
                            "content": { "type": "string", "minLength": 1, "maxLength": 200 },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "cancelled"]
                            }
                        }
                    }
                }
            }
        })
    }

    async fn run(
        &self,
        args: &Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError> {
        let next = TodoList::parse(args).map_err(AgentError::from)?;
        Ok(next.summary())
    }
}

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::Tool;
use crate::error::AgentError;
use crate::live::LiveHandle;
use crate::todo::TodoList;

pub struct TodoWriteTool {
    live: LiveHandle,
}

impl TodoWriteTool {
    pub fn new(live: LiveHandle) -> Self {
        Self { live }
    }
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "todo_write"
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
        {
            let mut g = self.live.lock().unwrap_or_else(|e| e.into_inner());
            g.todos = next.clone();
        }
        Ok(next.summary())
    }
}

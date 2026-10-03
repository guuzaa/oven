use std::sync::Arc;

use async_trait::async_trait;
use oven_agent::{AgentError, Tool, ToolCaps, ToolPermission, TurnContext};
use oven_mem::MemoryStore;
use serde_json::{Value, json};

use super::{parse_id, parse_scope};

const MISSING_SCOPE: &str = "memory_forget: missing 'scope' string argument";
const MISSING_ID: &str = "memory_forget: missing 'id' string argument";
const FORGOT_MEMORY: &str = "forgot memory";

pub struct MemoryForgetTool {
    store: Arc<MemoryStore>,
}

impl MemoryForgetTool {
    pub const NAME: &'static str = "memory_forget";

    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &'static str {
        "Delete a saved memory by scope and id. Fails when the id is unknown."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["workspace", "user"],
                    "description": "workspace for this repository, user for personal preferences"
                },
                "id": { "type": "string", "description": "Memory id to delete." }
            },
            "required": ["scope", "id"]
        })
    }

    fn caps(&self) -> ToolCaps {
        ToolCaps {
            permission: ToolPermission::Write,
            ..Default::default()
        }
    }

    async fn run(&self, args: &Value, _cx: &TurnContext) -> Result<String, AgentError> {
        let scope_raw = args
            .get("scope")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::from(MISSING_SCOPE))?;
        let id_raw = args
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::from(MISSING_ID))?;
        let id = parse_id(id_raw)?;
        let scope = parse_scope(Self::NAME, scope_raw)?;
        self.store
            .forget(scope, &id)
            .await
            .map_err(|err| AgentError::from(err.to_string()))?;
        Ok(format!("{FORGOT_MEMORY} {scope}/{id}"))
    }
}

#[cfg(test)]
mod tests {
    use oven_agent::{AgentMode, CancellationToken, Selection, Tool, TurnContext, TurnId};
    use oven_llm::ModelId;
    use oven_mem::{MemoryRoots, MemoryStore, NOT_FOUND};
    use serde_json::json;

    use super::{FORGOT_MEMORY, MemoryForgetTool};

    fn turn() -> TurnContext {
        TurnContext::new(
            TurnId::next(),
            CancellationToken::new(),
            Selection::new(AgentMode::Agent, ModelId::new("default"), None),
        )
    }

    async fn loaded(tmp: &tempdir::TempDir) -> std::sync::Arc<MemoryStore> {
        std::sync::Arc::new(
            MemoryStore::load(MemoryRoots {
                workspace: tmp.path().join("workspace"),
                user: None,
            })
            .await,
        )
    }

    #[tokio::test]
    async fn unknown_id_fails() {
        let tmp = tempdir::TempDir::new("memory-forget-tool").unwrap();
        let tool = MemoryForgetTool::new(loaded(&tmp).await);
        let err = tool
            .run(&json!({"scope": "workspace", "id": "missing"}), &turn())
            .await
            .unwrap_err();
        assert_eq!(err.message, format!("{NOT_FOUND}: workspace/missing"));
    }

    #[tokio::test]
    async fn deletes_the_file() {
        let tmp = tempdir::TempDir::new("memory-forget-ok").unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let path = workspace.join("proxy-requires-http2.md");
        std::fs::write(
            &path,
            "---\nkind: fact\ndescription: proxy fact\n---\n\nbody\n",
        )
        .unwrap();
        let store = MemoryStore::load(MemoryRoots {
            workspace: workspace.clone(),
            user: None,
        })
        .await;
        let tool = MemoryForgetTool::new(std::sync::Arc::new(store));
        let out = tool
            .run(
                &json!({"scope": "workspace", "id": "proxy-requires-http2"}),
                &turn(),
            )
            .await
            .unwrap();
        assert_eq!(
            out,
            format!("{FORGOT_MEMORY} workspace/proxy-requires-http2")
        );
        assert!(!path.exists());
    }
}

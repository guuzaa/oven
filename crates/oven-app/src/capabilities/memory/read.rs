use std::sync::Arc;

use async_trait::async_trait;
use oven_agent::{AgentError, Tool, ToolCaps, ToolPermission, ToolView, TurnContext};
use oven_mem::MemoryStore;
use serde_json::{Value, json};

use super::view::memory_view;
use super::{parse_id, parse_scope};

const MISSING_SCOPE: &str = "memory_read: missing 'scope' string argument";
const MISSING_ID: &str = "memory_read: missing 'id' string argument";
const KIND_LABEL: &str = "kind";
const DESCRIPTION_LABEL: &str = "description";

pub struct MemoryReadTool {
    store: Arc<MemoryStore>,
}

impl MemoryReadTool {
    pub const NAME: &'static str = "memory_read";
    pub const VERB: &'static str = "Recalled";

    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }

    pub fn view_input(input: &Value) -> ToolView {
        memory_view(Self::VERB, input)
    }
}

#[async_trait]
impl Tool for MemoryReadTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn view(&self, input: &Value) -> ToolView {
        Self::view_input(input)
    }

    fn description(&self) -> &'static str {
        "Read one saved memory by scope and id. The system prompt lists every memory; \
         this returns its kind, description and body."
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
                "id": { "type": "string", "description": "Memory id, e.g. \"proxy-requires-http2\"." }
            },
            "required": ["scope", "id"]
        })
    }

    fn caps(&self) -> ToolCaps {
        ToolCaps {
            permission: ToolPermission::Read,
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
        let memory = self
            .store
            .read(scope, &id)
            .await
            .map_err(|err| AgentError::from(err.to_string()))?;
        Ok(format!(
            "{KIND_LABEL}: {}\n{DESCRIPTION_LABEL}: {}\n\n{}",
            memory.kind.as_str(),
            memory.description,
            memory.body
        ))
    }
}

#[cfg(test)]
mod tests {
    use oven_agent::{AgentMode, CancellationToken, Selection, Tool, TurnContext, TurnId};
    use oven_llm::ModelId;
    use oven_mem::{INVALID_ID, MemoryRoots, MemoryStore, USER_SCOPE_UNAVAILABLE};
    use serde_json::json;

    use super::{DESCRIPTION_LABEL, KIND_LABEL, MemoryReadTool};

    fn turn() -> TurnContext {
        TurnContext::new(
            TurnId::next(),
            CancellationToken::new(),
            Selection::new(AgentMode::Agent, ModelId::new("default"), None),
        )
    }

    async fn store(user: bool) -> (tempdir::TempDir, MemoryReadTool) {
        let tmp = tempdir::TempDir::new("memory-read-tool").unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let user_root = user.then(|| tmp.path().join("user"));
        let loaded = MemoryStore::load(MemoryRoots {
            workspace,
            user: user_root,
        })
        .await;
        (tmp, MemoryReadTool::new(std::sync::Arc::new(loaded)))
    }

    #[tokio::test]
    async fn returns_a_body_edited_after_the_store_loaded() {
        let (tmp, tool) = store(false).await;
        let path = tmp.path().join("workspace").join("proxy-requires-http2.md");
        std::fs::write(
            &path,
            "---\nkind: fact\ndescription: proxy fact\n---\n\nfirst body\n",
        )
        .unwrap();
        std::fs::write(
            &path,
            "---\nkind: fact\ndescription: proxy fact\n---\n\nedited body\n",
        )
        .unwrap();
        let out = tool
            .run(
                &json!({"scope": "workspace", "id": "proxy-requires-http2"}),
                &turn(),
            )
            .await
            .unwrap();
        assert!(out.contains(&format!("{KIND_LABEL}: fact\n")));
        assert!(out.contains(&format!("{DESCRIPTION_LABEL}: proxy fact\n")));
        assert!(out.contains("edited body\n"));
        assert!(!out.contains("first body"));
    }

    #[tokio::test]
    async fn rejects_an_invalid_id_before_reading() {
        let (tmp, tool) = store(false).await;
        let err = tool
            .run(&json!({"scope": "workspace", "id": "../x"}), &turn())
            .await
            .unwrap_err();
        assert_eq!(err.message, INVALID_ID);
        assert!(!tmp.path().join("x.md").exists());
    }

    #[tokio::test]
    async fn user_scope_unavailable_is_a_tool_error() {
        let (_tmp, tool) = store(false).await;
        let err = tool
            .run(&json!({"scope": "user", "id": "shared-id"}), &turn())
            .await
            .unwrap_err();
        assert_eq!(err.message, USER_SCOPE_UNAVAILABLE);
    }
}

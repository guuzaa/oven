use std::sync::Arc;

use async_trait::async_trait;
use oven_agent::{Tool, ToolCaps, ToolOutcome, TurnContext};
use oven_mem::{MemoryScope, MemoryStore, SHARED_READ_ONLY};
use oven_proto::{AgentError, ToolPermission, ToolView};
use serde_json::{Value, json};

use super::view::memory_view;
use super::{parse_ref, ref_property};

const FORGOT_MEMORY: &str = "forgot memory";

pub struct MemoryForgetTool {
    store: Arc<MemoryStore>,
}

impl MemoryForgetTool {
    pub const NAME: &'static str = "memory_forget";
    pub const VERB: &'static str = "Forgot";

    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn view(&self, input: &Value) -> ToolView {
        memory_view(Self::VERB, input)
    }

    fn description(&self) -> &'static str {
        "Delete a saved memory, only when the user asks you to forget it. A memory that \
         turned out wrong gets a memory_note with about set instead. Fails when the ref \
         is unknown or in the shared layer."
    }

    fn schema(&self) -> Arc<Value> {
        Arc::new(json!({
            "type": "object",
            "properties": { "ref": ref_property() },
            "required": ["ref"]
        }))
    }

    fn caps(&self) -> ToolCaps {
        ToolCaps {
            permission: ToolPermission::Write,
            ..Default::default()
        }
    }

    async fn run(&self, args: &Value, _cx: &TurnContext) -> Result<ToolOutcome, AgentError> {
        let (scope, id) = parse_ref(args, Self::NAME)?;
        if scope == MemoryScope::Shared {
            return Err(AgentError::from(format!(
                "{}: {SHARED_READ_ONLY}",
                Self::NAME
            )));
        }
        self.store
            .forget(scope, &id)
            .await
            .map_err(|err| AgentError::from(err.to_string()))?;
        Ok(format!("{FORGOT_MEMORY} {scope}/{id}").into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use oven_agent::{CancellationToken, Selection, Tool, TurnContext};
    use oven_llm::ModelId;
    use oven_mem::{MemoryRoots, MemoryStore, NOT_FOUND, SHARED_READ_ONLY};
    use oven_proto::{AgentMode, TurnId};
    use serde_json::json;

    use super::{FORGOT_MEMORY, MemoryForgetTool};

    const FACT: &str = "---\ntype: Fact\ndescription: proxy fact\n---\n\nbody\n";

    fn turn() -> TurnContext {
        TurnContext::new(
            TurnId::next(),
            CancellationToken::new(),
            Selection::new(AgentMode::Agent, ModelId::new("default"), None),
        )
    }

    async fn tool(tmp: &tempdir::TempDir) -> MemoryForgetTool {
        for dir in ["project", "shared"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
            std::fs::write(tmp.path().join(dir).join("proxy.md"), FACT).unwrap();
        }
        let store = MemoryStore::load(MemoryRoots {
            workspace: tmp.path().to_path_buf(),
            project: Some(tmp.path().join("project")),
            shared: tmp.path().join("shared"),
            global: None,
            inbox: None,
        })
        .await;
        MemoryForgetTool::new(Arc::new(store))
    }

    #[tokio::test]
    async fn forgets_a_private_memory_and_refuses_shared() {
        let tmp = tempdir::TempDir::new("memory-forget").unwrap();
        let tool = tool(&tmp).await;
        let out = tool
            .run(&json!({"ref": "project/proxy"}), &turn())
            .await
            .unwrap();
        assert_eq!(out.output, format!("{FORGOT_MEMORY} project/proxy"));
        assert!(!tmp.path().join("project").join("proxy.md").exists());

        let err = tool
            .run(&json!({"ref": "shared/proxy"}), &turn())
            .await
            .unwrap_err();
        assert_eq!(err.message, format!("memory_forget: {SHARED_READ_ONLY}"));
        assert!(tmp.path().join("shared").join("proxy.md").exists());

        let err = tool
            .run(&json!({"ref": "project/proxy"}), &turn())
            .await
            .unwrap_err();
        assert_eq!(err.message, format!("{NOT_FOUND}: project/proxy"));
    }
}

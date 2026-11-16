use std::sync::Arc;

use async_trait::async_trait;
use oven_agent::{Tool, ToolCaps, ToolOutcome, TurnContext};
use oven_mem::MemoryStore;
use oven_proto::{AgentError, ToolPermission, ToolView};
use serde_json::{Value, json};

use super::view::memory_view;
use super::{parse_ref, ref_property};
use crate::core::memory::describe;

pub struct MemoryReadTool {
    store: Arc<MemoryStore>,
}

impl MemoryReadTool {
    pub const NAME: &'static str = "memory_read";
    pub const VERB: &'static str = "Recalled";

    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for MemoryReadTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn view(&self, input: &Value) -> ToolView {
        memory_view(Self::VERB, input)
    }

    fn description(&self) -> &'static str {
        "Read one saved memory by the ref the catalog lists it under. Returns its type, \
         description, reason, anchors and body."
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
            permission: ToolPermission::Read,
            ..Default::default()
        }
    }

    async fn run(&self, args: &Value, _cx: &TurnContext) -> Result<ToolOutcome, AgentError> {
        let (scope, id) = parse_ref(args, Self::NAME)?;
        let memory = self
            .store
            .read(scope, &id)
            .await
            .map_err(|err| AgentError::from(err.to_string()))?;
        Ok(describe(&memory).into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use oven_agent::{CancellationToken, Selection, Tool, ToolOutcome, TurnContext};
    use oven_llm::ModelId;
    use oven_mem::{INVALID_ID, LAYER_UNAVAILABLE, MemoryRoots, MemoryStore};
    use oven_proto::{AgentMode, TurnId};
    use serde_json::json;

    use super::MemoryReadTool;
    use crate::core::memory::{DESCRIPTION_LABEL, TYPE_LABEL, UNKNOWN_SCOPE};

    fn turn() -> TurnContext {
        TurnContext::new(
            TurnId::next(),
            CancellationToken::new(),
            Selection::new(AgentMode::Agent, ModelId::new("default"), None),
        )
    }

    async fn tool(tmp: &tempdir::TempDir, home: bool) -> MemoryReadTool {
        let root = tmp.path().to_path_buf();
        let store = MemoryStore::load(MemoryRoots {
            workspace: root.clone(),
            project: home.then(|| root.join("project")),
            shared: root.join("shared"),
            global: home.then(|| root.join("global")),
            inbox: None,
        })
        .await;
        MemoryReadTool::new(Arc::new(store))
    }

    #[tokio::test]
    async fn returns_a_body_edited_after_the_store_loaded() {
        let tmp = tempdir::TempDir::new("memory-read-tool").unwrap();
        let tool = tool(&tmp, true).await;
        let dir = tmp.path().join("project");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("proxy.md"),
            "---\ntype: Fact\ndescription: proxy fact\n---\n\nedited body\n",
        )
        .unwrap();
        let ToolOutcome { output, .. } = tool
            .run(&json!({"ref": "project/proxy"}), &turn())
            .await
            .unwrap();
        assert!(output.contains(&format!("{TYPE_LABEL}: fact\n")));
        assert!(output.contains(&format!("{DESCRIPTION_LABEL}: proxy fact\n")));
        assert!(output.ends_with("\nedited body\n"));
    }

    #[tokio::test]
    async fn rejects_a_bad_ref_before_reading() {
        let tmp = tempdir::TempDir::new("memory-read-bad").unwrap();
        let tool = tool(&tmp, true).await;
        let err = tool
            .run(&json!({"ref": "project/../x"}), &turn())
            .await
            .unwrap_err();
        assert_eq!(err.message, INVALID_ID);
        let err = tool
            .run(&json!({"ref": "user/x"}), &turn())
            .await
            .unwrap_err();
        assert!(err.message.contains(UNKNOWN_SCOPE), "{}", err.message);
    }

    #[tokio::test]
    async fn a_layer_without_a_home_is_a_tool_error() {
        let tmp = tempdir::TempDir::new("memory-read-no-home").unwrap();
        let tool = tool(&tmp, false).await;
        let err = tool
            .run(&json!({"ref": "global/x"}), &turn())
            .await
            .unwrap_err();
        assert_eq!(err.message, format!("{LAYER_UNAVAILABLE}: global"));
    }
}

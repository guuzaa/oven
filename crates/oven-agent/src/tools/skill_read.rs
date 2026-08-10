//! The `skill_read` tool: dynamically loads the full guidance document of a
//! skill from disk on demand.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::Tool;
use crate::error::AgentError;

/// Reads the full content of a skill's `SKILL.md` by id. The system prompt
/// only lists skill descriptions; this tool is how the model gets the body.
pub struct SkillReadTool {
    sources: Arc<BTreeMap<String, PathBuf>>,
}

impl SkillReadTool {
    pub fn new(sources: Arc<BTreeMap<String, PathBuf>>) -> Self {
        Self { sources }
    }
}

#[async_trait]
impl Tool for SkillReadTool {
    fn name(&self) -> &str {
        "skill_read"
    }

    fn description(&self) -> &str {
        "Load the full content of a skill by id. Skills are guidance modules; \
         the system prompt only lists their descriptions."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "skill_id": { "type": "string", "description": "Skill id, e.g. \"files\"." }
            },
            "required": ["skill_id"]
        })
    }

    async fn run(
        &self,
        args: &Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError> {
        let id = args
            .get("skill_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentError::from("skill_read: missing 'skill_id' string argument"))?;
        let path = self
            .sources
            .get(id)
            .ok_or_else(|| AgentError::from(format!("unknown skill: {id}")))?;
        std::fs::read_to_string(path)
            .map_err(|e| AgentError::from(format!("read {}: {}", path.display(), e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::SKILL_FILE;
    use serde_json::json;

    #[tokio::test]
    async fn reads_skill_content_dynamically() {
        let tmp = tempdir::TempDir::new("skill-tool").unwrap();
        let file = tmp.path().join(SKILL_FILE);
        std::fs::write(&file, "full body\n").unwrap();
        let sources = Arc::new(BTreeMap::from([("files".to_string(), file.clone())]));
        let tool = SkillReadTool::new(sources);

        let out = tool.run(&json!({"skill_id": "files"}), None).await.unwrap();
        assert_eq!(out, "full body\n");

        // Content is re-read from disk on every call.
        std::fs::write(&file, "updated body\n").unwrap();
        let out = tool.run(&json!({"skill_id": "files"}), None).await.unwrap();
        assert_eq!(out, "updated body\n");
    }

    #[tokio::test]
    async fn unknown_skill_errors() {
        let tool = SkillReadTool::new(Arc::new(BTreeMap::new()));
        let err = tool
            .run(&json!({"skill_id": "nope"}), None)
            .await
            .unwrap_err();
        assert!(err.message.contains("unknown skill"));
    }
}

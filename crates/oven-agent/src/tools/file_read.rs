use std::fs;
use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, require_str, resolve_within};
use crate::error::AgentError;

pub struct FileReadTool {
    root: PathBuf,
}

impl FileReadTool {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }
    fn description(&self) -> &str {
        "Read the contents of a UTF-8 text file as a string."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the workspace root." }
            },
            "required": ["path"]
        })
    }
    async fn run(
        &self,
        args: &Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError> {
        let path_str = require_str(args, "path", "file_read")?;
        let path = resolve_within(&self.root, path_str)?;
        if !path.is_file() {
            return Err(AgentError::from(format!("not a file: {}", path.display())));
        }
        fs::read_to_string(&path)
            .map_err(|e| AgentError::from(format!("read {}: {}", path.display(), e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp_dir() -> tempdir::TempDir {
        tempdir::TempDir::new("oven-test").unwrap()
    }

    #[tokio::test]
    async fn tools_reject_path_escape() {
        let tmp = tmp_dir();
        let read = FileReadTool::new(tmp.path());
        let err = read
            .run(&json!({"path": "../etc/passwd"}), None)
            .await
            .unwrap_err();
        assert!(err.message.contains("escapes root"));
    }
}

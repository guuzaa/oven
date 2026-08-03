use std::fs;
use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, require_str, resolve_within};
use crate::error::AgentError;

pub struct FileWriteTool {
    root: PathBuf,
}

impl FileWriteTool {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }
    fn description(&self) -> &str {
        "Write text content to a file, creating parent directories as needed. Overwrites."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the workspace root." },
                "content": { "type": "string", "description": "The text content to write." }
            },
            "required": ["path", "content"]
        })
    }
    async fn run(
        &self,
        args: &Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError> {
        let path_str = require_str(args, "path", "file_write")?;
        let content = require_str(args, "content", "file_write")?;
        let path = resolve_within(&self.root, path_str)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                AgentError::from(format!("create dirs {}: {}", parent.display(), e))
            })?;
        }
        fs::write(&path, content)
            .map_err(|e| AgentError::from(format!("write {}: {}", path.display(), e)))?;
        Ok(format!(
            "wrote {} bytes to {}",
            content.len(),
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::FileReadTool;
    use super::*;
    use serde_json::json;

    fn tmp_dir() -> tempdir::TempDir {
        tempdir::TempDir::new("oven-test").unwrap()
    }

    #[tokio::test]
    async fn file_write_then_read() {
        let tmp = tmp_dir();
        let root = tmp.path();
        let write = FileWriteTool::new(root);
        let out = write
            .run(
                &json!({"path": "hello.txt", "content": "line one\nline two"}),
                None,
            )
            .await
            .unwrap();
        assert!(out.contains("wrote"));
        let read = FileReadTool::new(root);
        let content = read.run(&json!({"path": "hello.txt"}), None).await.unwrap();
        assert_eq!(content, "line one\nline two");
    }
}

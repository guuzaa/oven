use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolView, require_str, resolve_within};
use crate::error::AgentError;

pub struct FileWriteTool {
    root: PathBuf,
}

impl FileWriteTool {
    pub const NAME: &'static str = "file_write";

    pub fn view_input(input: &Value) -> ToolView {
        let Some(path) = input.get("path").and_then(Value::as_str) else {
            return ToolView::named(Self::NAME);
        };
        let Some(content) = input.get("content").and_then(Value::as_str) else {
            return ToolView::named(Self::NAME);
        };

        let mut diff = format!("Write {}", path.trim());
        for line in content.split('\n') {
            diff.push_str("\n+ ");
            diff.push_str(line.trim_end_matches('\r'));
        }

        ToolView {
            summary: diff,
            collapse: false,
            diff: true,
        }
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn view(&self, input: &Value) -> ToolView {
        Self::view_input(input)
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
        let path_str = require_str(args, "path", Self::NAME)?;
        let content = require_str(args, "content", Self::NAME)?;
        let path = resolve_within(&self.root, path_str)?;
        oven_host::write(&path, content)
            .await
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

    #[test]
    fn view_shows_content_as_diff() {
        let view = FileWriteTool::view_input(&json!({
            "path": "hello.txt",
            "content": "line one\nline two",
        }));
        assert!(!view.collapse);
        assert_eq!(view.summary, "Write hello.txt\n+ line one\n+ line two");
    }

    #[test]
    fn view_falls_back_without_content() {
        assert_eq!(
            FileWriteTool::view_input(&json!({ "path": "hello.txt" })).summary,
            FileWriteTool::NAME
        );
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
        assert_eq!(
            content,
            "file: hello.txt\nlines: 1-2\n\nL1→line one\nL2→line two"
        );
    }
}

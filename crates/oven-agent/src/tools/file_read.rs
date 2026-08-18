use std::path::PathBuf;
use tokio::fs;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolView, labeled, require_str, resolve_within};
use crate::error::AgentError;

pub struct FileReadTool {
    root: PathBuf,
}

impl FileReadTool {
    pub const NAME: &'static str = "file_read";

    pub fn view_input(input: &Value) -> ToolView {
        labeled(Self::NAME, "Read", input, "path")
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn view(&self, input: &Value) -> ToolView {
        Self::view_input(input)
    }
    fn description(&self) -> &str {
        "Read the contents of a UTF-8 text file as a string. Optionally restrict \
         to a range of lines with `offset` (1-based) and `limit` to reduce output."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the workspace root." },
                "offset": { "type": "integer", "description": "1-based first line to read. Defaults to 1." },
                "limit": { "type": "integer", "description": "Maximum number of lines to read. Defaults to all." }
            },
            "required": ["path"]
        })
    }
    async fn run(
        &self,
        args: &Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError> {
        let path_str = require_str(args, "path", Self::NAME)?;
        let path = resolve_within(&self.root, path_str)?;
        if !path.is_file() {
            return Err(AgentError::from(format!("not a file: {}", path.display())));
        }
        let content = fs::read_to_string(&path)
            .await
            .map_err(|e| AgentError::from(format!("read {}: {}", path.display(), e)))?;

        let offset = args.get("offset").and_then(|v| v.as_i64()).unwrap_or(1);
        let limit = args.get("limit").and_then(|v| v.as_i64());

        let lines: Vec<&str> = content.split_inclusive('\n').collect();
        let total = lines.len();
        let start = (offset.max(1) as usize).saturating_sub(1).min(total);
        let end = match limit {
            Some(n) if n > 0 => start.saturating_add(n as usize).min(total),
            _ => total,
        };
        if start >= total {
            return Ok(String::new());
        }
        Ok(lines[start..end].concat())
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

    #[tokio::test]
    async fn reads_line_range() {
        let tmp = tmp_dir();
        let path = tmp.path().join("r.txt");
        std::fs::write(&path, "l1\nl2\nl3\nl4\n").unwrap();
        let read = FileReadTool::new(tmp.path());
        let out = read
            .run(&json!({"path": "r.txt", "offset": 2, "limit": 2}), None)
            .await
            .unwrap();
        assert_eq!(out, "l2\nl3\n");
    }

    #[tokio::test]
    async fn offset_beyond_end_returns_empty() {
        let tmp = tmp_dir();
        let path = tmp.path().join("r.txt");
        std::fs::write(&path, "l1\nl2\n").unwrap();
        let read = FileReadTool::new(tmp.path());
        let out = read
            .run(&json!({"path": "r.txt", "offset": 99, "limit": 5}), None)
            .await
            .unwrap();
        assert_eq!(out, "");
    }

    #[tokio::test]
    async fn full_read_is_unchanged() {
        let tmp = tmp_dir();
        let path = tmp.path().join("r.txt");
        std::fs::write(&path, "one\ntwo").unwrap();
        let read = FileReadTool::new(tmp.path());
        let out = read.run(&json!({"path": "r.txt"}), None).await.unwrap();
        assert_eq!(out, "one\ntwo");
    }

    #[tokio::test]
    async fn offset_without_limit_reads_to_end() {
        let tmp = tmp_dir();
        let path = tmp.path().join("r.txt");
        std::fs::write(&path, "l1\nl2\nl3\nl4\n").unwrap();
        let read = FileReadTool::new(tmp.path());
        let out = read
            .run(&json!({"path": "r.txt", "offset": 2}), None)
            .await
            .unwrap();
        assert_eq!(out, "l2\nl3\nl4\n");
    }
}

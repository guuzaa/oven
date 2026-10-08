use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Tool, ToolCaps, ToolPermission, ToolView, labeled, require_str, resolve_within};

use crate::core::error::AgentError;
use crate::core::turn::TurnContext;

pub struct FileWriteTool {
    root: PathBuf,
}

impl FileWriteTool {
    pub const NAME: &'static str = "file_write";
    pub const VERB: &'static str = "Wrote";

    pub fn view_input(input: &Value) -> ToolView {
        let mut view = labeled(Self::VERB, input, "path");
        if let Some(content) = input.get("content").and_then(Value::as_str) {
            view.detail = Some(added_lines(content));
        }
        view
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

fn added_lines(content: &str) -> String {
    content
        .lines()
        .map(|line| format!("+ {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn view(&self, input: &Value) -> ToolView {
        Self::view_input(input)
    }
    fn caps(&self) -> ToolCaps {
        ToolCaps {
            permission: ToolPermission::Write,
            exclusive: true,
            ..Default::default()
        }
    }
    fn description(&self) -> &'static str {
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
    async fn run(&self, args: &Value, _cx: &TurnContext) -> Result<String, AgentError> {
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
    fn turn() -> TurnContext {
        TurnContext::for_test()
    }
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
        assert!(view.collapse);
        assert_eq!(view.summary, format!("{} hello.txt", FileWriteTool::VERB));
        assert_eq!(view.detail.as_deref(), Some("+ line one\n+ line two"));
    }

    #[test]
    fn view_falls_back_to_the_verb() {
        let path_only = FileWriteTool::view_input(&json!({ "path": "hello.txt" }));
        assert_eq!(
            path_only.summary,
            format!("{} hello.txt", FileWriteTool::VERB)
        );
        assert_eq!(path_only.detail, None);

        let no_args = FileWriteTool::view_input(&json!({}));
        assert_eq!(no_args.summary, FileWriteTool::VERB);
        assert_eq!(no_args.detail, None);
        assert_eq!(
            FileWriteTool::view_input(&json!({ "path": "hello.txt", "content": 7 })).detail,
            None
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
                &turn(),
            )
            .await
            .unwrap();
        assert!(out.contains("wrote"));
        let read = FileReadTool::new(root);
        let content = read
            .run(&json!({"path": "hello.txt"}), &turn())
            .await
            .unwrap();
        assert_eq!(
            content,
            "file: hello.txt\nlines: 1-2\n\nL1→line one\nL2→line two"
        );
    }
}

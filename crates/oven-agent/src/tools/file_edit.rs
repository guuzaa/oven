use std::path::PathBuf;
use tokio::fs;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolView, labeled, require_str, resolve_within};
use crate::error::AgentError;

pub struct FileEditTool {
    root: PathBuf,
}

impl FileEditTool {
    pub const NAME: &'static str = "file_edit";

    pub fn view_input(input: &Value) -> ToolView {
        labeled(Self::NAME, "Edited", input, "path")
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn view(&self, input: &Value) -> ToolView {
        Self::view_input(input)
    }
    fn description(&self) -> &str {
        "Edit a file by replacing text. Replaces the single exact occurrence of \
         old_string with new_string; fails if old_string matches more than once \
         unless replace_all is true."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the workspace root." },
                "old_string": { "type": "string", "description": "The exact text to replace." },
                "new_string": { "type": "string", "description": "The replacement text." },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence of old_string when true." }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    async fn run(
        &self,
        args: &Value,
        _cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError> {
        let path_str = require_str(args, "path", Self::NAME)?;
        let old_string = require_str(args, "old_string", Self::NAME)?;
        let new_string = require_str(args, "new_string", Self::NAME)?;
        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let path = resolve_within(&self.root, path_str)?;
        if !path.is_file() {
            return Err(AgentError::from(format!("not a file: {}", path.display())));
        }
        let content = fs::read_to_string(&path)
            .await
            .map_err(|e| AgentError::from(format!("read {}: {}", path.display(), e)))?;

        let matches = content.matches(old_string).count();
        if matches == 0 {
            return Err(AgentError::from(format!(
                "old_string not found in {}",
                path.display()
            )));
        }
        if !replace_all && matches != 1 {
            return Err(AgentError::from(format!(
                "old_string matched {} times in {}; expected exactly 1 (or set replace_all)",
                matches,
                path.display()
            )));
        }

        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };
        fs::write(&path, &new_content)
            .await
            .map_err(|e| AgentError::from(format!("write {}: {}", path.display(), e)))?;
        Ok(format!(
            "edited {}: replaced {} occurrence(s), {} bytes -> {} bytes",
            path.display(),
            matches,
            content.len(),
            new_content.len()
        ))
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
    async fn replaces_single_occurrence() {
        let tmp = tmp_dir();
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, "one two three").unwrap();
        let edit = FileEditTool::new(tmp.path());
        edit.run(
            &json!({"path": "a.txt", "old_string": "two", "new_string": "2"}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one 2 three");
    }

    #[tokio::test]
    async fn rejects_ambiguous_match_without_replace_all() {
        let tmp = tmp_dir();
        std::fs::write(tmp.path().join("a.txt"), "a a").unwrap();
        let edit = FileEditTool::new(tmp.path());
        let err = edit
            .run(
                &json!({"path": "a.txt", "old_string": "a", "new_string": "b"}),
                None,
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("matched 2 times"), "{}", err.message);
    }

    #[tokio::test]
    async fn replace_all_replaces_every_occurrence() {
        let tmp = tmp_dir();
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, "a a a").unwrap();
        let edit = FileEditTool::new(tmp.path());
        edit.run(
            &json!({"path": "a.txt", "old_string": "a", "new_string": "b", "replace_all": true}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b b b");
    }

    #[tokio::test]
    async fn missing_old_string_errors() {
        let tmp = tmp_dir();
        std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        let edit = FileEditTool::new(tmp.path());
        let err = edit
            .run(
                &json!({"path": "a.txt", "old_string": "zzz", "new_string": "x"}),
                None,
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("not found"), "{}", err.message);
    }

    #[tokio::test]
    async fn rejects_path_escape() {
        let tmp = tmp_dir();
        std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        let edit = FileEditTool::new(tmp.path());
        let err = edit
            .run(
                &json!({"path": "../a.txt", "old_string": "a", "new_string": "b"}),
                None,
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("escapes root"), "{}", err.message);
    }
}

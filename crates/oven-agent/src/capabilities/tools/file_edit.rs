use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{Tool, ToolCaps, ToolPermission, ToolView, labeled, require_str, resolve_within};

use crate::core::error::AgentError;
use crate::core::turn::TurnContext;

pub struct FileEditTool {
    root: PathBuf,
}

struct Edit<'a> {
    old_string: &'a str,
    new_string: &'a str,
}

impl FileEditTool {
    pub const NAME: &'static str = "file_edit";
    pub const VERB: &'static str = "Edited";

    /// The path alone still reads as an edit; the diff only appears once every
    /// string parses, so a broken call shows the error where the diff would be.
    pub fn view_input(input: &Value) -> ToolView {
        let mut view = labeled(Self::VERB, input, "path");
        if let Ok(edits) = Self::parse_edits(input) {
            view.detail = Some(
                edits
                    .iter()
                    .map(|edit| diff_lines(edit.old_string, edit.new_string))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        view
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn parse_edits(args: &Value) -> Result<Vec<Edit<'_>>, AgentError> {
        let items = args
            .get("edit")
            .and_then(Value::as_array)
            .ok_or_else(|| AgentError::from(format!("{}: 'edit' must be an array", Self::NAME)))?;
        if items.is_empty() {
            return Err(AgentError::from(format!(
                "{}: 'edit' must hold at least one edit",
                Self::NAME
            )));
        }
        items
            .iter()
            .enumerate()
            .map(|(index, item)| Self::parse_edit(item, index))
            .collect()
    }

    fn parse_edit(item: &Value, index: usize) -> Result<Edit<'_>, AgentError> {
        let string = |key: &str| {
            item.get(key).and_then(Value::as_str).ok_or_else(|| {
                AgentError::from(format!(
                    "{}: edit[{index}] is missing '{key}' string argument",
                    Self::NAME
                ))
            })
        };
        Ok(Edit {
            old_string: string("old_string")?,
            new_string: string("new_string")?,
        })
    }
}

fn apply_edits(
    content: &str,
    edits: &[Edit<'_>],
    path: &Path,
) -> Result<(String, usize), AgentError> {
    let mut content = content.to_string();
    let mut replacements = 0;
    for (index, edit) in edits.iter().enumerate() {
        let matches = content.matches(edit.old_string).count();
        if matches == 0 {
            return Err(AgentError::from(format!(
                "edit[{index}]: old_string not found in {}",
                path.display()
            )));
        }
        if matches != 1 {
            return Err(AgentError::from(format!(
                "edit[{index}]: old_string matched {} times in {}; \
                 add more surrounding context so it matches exactly once",
                matches,
                path.display()
            )));
        }
        replacements += 1;
        content = content.replacen(edit.old_string, edit.new_string, 1);
    }
    Ok((content, replacements))
}

fn diff_lines(old_string: &str, new_string: &str) -> String {
    let removed = old_string.lines().map(|line| format!("- {line}"));
    let added = new_string.lines().map(|line| format!("+ {line}"));
    removed.chain(added).collect::<Vec<_>>().join("\n")
}

#[async_trait]
impl Tool for FileEditTool {
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
        "Edit a file by replacing text. `edit` is an array of {old_string, \
         new_string} objects applied in order; a single-element array is one \
         edit. Each old_string must match exactly once, so include enough \
         surrounding context to make it unique."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the workspace root." },
                "edit": {
                    "type": "array",
                    "description": "Edits to apply to the file, in order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string", "description": "The exact text to replace; it must match exactly once." },
                            "new_string": { "type": "string", "description": "The replacement text." }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["path", "edit"]
        })
    }
    async fn run(&self, args: &Value, _cx: &TurnContext) -> Result<String, AgentError> {
        let path_str = require_str(args, "path", Self::NAME)?;
        let edits = Self::parse_edits(args)?;

        let path = resolve_within(&self.root, path_str)?;
        if !path.is_file() {
            return Err(AgentError::from(format!("not a file: {}", path.display())));
        }
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| AgentError::from(format!("read {}: {}", path.display(), e)))?;

        let (new_content, replacements) = apply_edits(&content, &edits, &path)?;
        oven_host::write(&path, &new_content)
            .await
            .map_err(|e| AgentError::from(format!("write {}: {}", path.display(), e)))?;
        Ok(format!(
            "edited {}: {} edit(s), {} replacement(s), {} bytes -> {} bytes",
            path.display(),
            edits.len(),
            replacements,
            content.len(),
            new_content.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn turn() -> TurnContext {
        TurnContext::for_test()
    }
    use serde_json::json;

    fn tmp_dir() -> tempdir::TempDir {
        tempdir::TempDir::new("oven-test").unwrap()
    }

    #[test]
    fn view_shows_edit_as_diff() {
        let view = FileEditTool::view_input(&json!({
            "path": "src/main.rs",
            "edit": [{ "old_string": "let answer = 41;", "new_string": "let answer = 42;" }]
        }));
        assert!(view.collapse);
        assert_eq!(view.summary, format!("{} src/main.rs", FileEditTool::VERB));
        assert_eq!(
            view.detail.as_deref(),
            Some("- let answer = 41;\n+ let answer = 42;")
        );
    }

    #[test]
    fn view_falls_back_to_the_verb() {
        let path_only = FileEditTool::view_input(&json!({ "path": "src/main.rs" }));
        assert_eq!(
            path_only.summary,
            format!("{} src/main.rs", FileEditTool::VERB)
        );
        assert_eq!(path_only.detail, None);

        let no_args = FileEditTool::view_input(&json!({}));
        assert_eq!(no_args.summary, FileEditTool::VERB);
        assert_eq!(no_args.detail, None);
        assert_eq!(
            FileEditTool::view_input(
                &json!({ "path": "src/main.rs", "edit": [{ "old_string": 41 }] })
            )
            .detail,
            None
        );
    }

    #[tokio::test]
    async fn replaces_single_occurrence() {
        let tmp = tmp_dir();
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, "one two three").unwrap();
        let edit = FileEditTool::new(tmp.path());
        edit.run(
            &json!({"path": "a.txt", "edit": [{ "old_string": "two", "new_string": "2" }]}),
            &turn(),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one 2 three");
    }

    #[tokio::test]
    async fn rejects_ambiguous_match() {
        let tmp = tmp_dir();
        std::fs::write(tmp.path().join("a.txt"), "a a").unwrap();
        let edit = FileEditTool::new(tmp.path());
        let err = edit
            .run(
                &json!({"path": "a.txt", "edit": [{ "old_string": "a", "new_string": "b" }]}),
                &turn(),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("matched 2 times"), "{}", err.message);
    }

    #[tokio::test]
    async fn each_repeated_site_needs_its_own_edit() {
        let tmp = tmp_dir();
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, "a a").unwrap();
        let edit = FileEditTool::new(tmp.path());
        let err = edit
            .run(
                &json!({
                    "path": "a.txt",
                    "edit": [
                        { "old_string": "a", "new_string": "b" },
                        { "old_string": "a", "new_string": "c" },
                    ]
                }),
                &turn(),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("edit[0]"), "{}", err.message);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a a");
    }

    #[tokio::test]
    async fn missing_old_string_errors() {
        let tmp = tmp_dir();
        std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        let edit = FileEditTool::new(tmp.path());
        let err = edit
            .run(
                &json!({"path": "a.txt", "edit": [{ "old_string": "zzz", "new_string": "x" }]}),
                &turn(),
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
                &json!({"path": "../a.txt", "edit": [{ "old_string": "a", "new_string": "b" }]}),
                &turn(),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("escapes root"), "{}", err.message);
    }

    #[test]
    fn view_stacks_diffs_of_every_edit() {
        let view = FileEditTool::view_input(&json!({
            "path": "src/main.rs",
            "edit": [
                { "old_string": "one", "new_string": "1" },
                { "old_string": "two", "new_string": "2" },
            ]
        }));
        assert_eq!(view.detail.as_deref(), Some("- one\n+ 1\n- two\n+ 2"));
    }

    #[tokio::test]
    async fn edit_array_applies_every_edit_in_order() {
        let tmp = tmp_dir();
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, "alpha beta gamma").unwrap();
        let edit = FileEditTool::new(tmp.path());
        edit.run(
            &json!({
                "path": "a.txt",
                "edit": [
                    { "old_string": "alpha", "new_string": "one" },
                    { "old_string": "beta", "new_string": "two" },
                    { "old_string": "gamma", "new_string": "three" },
                ]
            }),
            &turn(),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one two three");
    }

    #[tokio::test]
    async fn edit_array_sees_earlier_edits() {
        let tmp = tmp_dir();
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, "a b").unwrap();
        let edit = FileEditTool::new(tmp.path());
        edit.run(
            &json!({
                "path": "a.txt",
                "edit": [
                    { "old_string": "a", "new_string": "b" },
                    { "old_string": "b b", "new_string": "c" },
                ]
            }),
            &turn(),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "c");
    }

    #[tokio::test]
    async fn edit_array_writes_nothing_when_one_edit_fails() {
        let tmp = tmp_dir();
        let path = tmp.path().join("a.txt");
        std::fs::write(&path, "alpha").unwrap();
        let edit = FileEditTool::new(tmp.path());
        let err = edit
            .run(
                &json!({
                    "path": "a.txt",
                    "edit": [
                        { "old_string": "alpha", "new_string": "beta" },
                        { "old_string": "zzz", "new_string": "x" },
                    ]
                }),
                &turn(),
            )
            .await
            .unwrap_err();
        assert!(
            err.message.contains("edit[1]: old_string not found"),
            "{}",
            err.message
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha");
    }

    #[tokio::test]
    async fn edit_array_rejects_empty_and_malformed_entries() {
        let tmp = tmp_dir();
        std::fs::write(tmp.path().join("a.txt"), "alpha").unwrap();
        let edit = FileEditTool::new(tmp.path());
        for args in [
            json!({"path": "a.txt", "edit": []}),
            json!({"path": "a.txt", "edit": "alpha"}),
            json!({"path": "a.txt", "edit": [{"old_string": "alpha"}]}),
        ] {
            let err = edit.run(&args, &turn()).await.unwrap_err();
            assert!(
                err.message.contains("edit"),
                "unexpected message for {args}: {}",
                err.message
            );
        }
    }
}

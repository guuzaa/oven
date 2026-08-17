use std::path::PathBuf;

use async_trait::async_trait;
use globset::Glob;
use ignore::WalkBuilder;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, require_str, resolve_within};
use crate::error::AgentError;

pub struct GlobTool {
    root: PathBuf,
    max_results: usize,
}

impl GlobTool {
    pub const NAME: &'static str = "glob";

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_results: 100,
        }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn description(&self) -> &str {
        "Find files by glob pattern. Patterns support `**` for recursive matches. \
         Results are paths relative to the workspace root, usable directly by \
         file_read/file_edit. Respects .gitignore."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, e.g. \"**/*.rs\" or \"src/**/*.md\"." },
                "path": { "type": "string", "description": "Directory to search within, relative to the workspace root. Defaults to root." },
                "limit": { "type": "integer", "description": "Maximum number of results to return. Defaults to 100." }
            },
            "required": ["pattern"]
        })
    }
    async fn run(
        &self,
        args: &Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError> {
        let pattern = require_str(args, "pattern", Self::NAME)?;
        let matcher = Glob::new(pattern)
            .map_err(|e| AgentError::from(format!("glob: invalid pattern {:?}: {}", pattern, e)))?
            .compile_matcher();

        let base_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(".");
        let base = resolve_within(&self.root, base_str)?;
        if !base.is_dir() {
            return Err(AgentError::from(format!(
                "glob: not a directory: {}",
                base.display()
            )));
        }
        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .map(|v| v.max(0) as usize)
            .unwrap_or(self.max_results);

        let mut matches = Vec::new();
        for entry in WalkBuilder::new(&base).require_git(false).build() {
            if matches.len() >= limit {
                break;
            }
            if let Some(c) = cancel
                && c.is_cancelled()
            {
                return Err(AgentError::cancelled());
            }
            let entry = entry.map_err(|e| AgentError::from(format!("glob: walk: {}", e)))?;
            if entry.file_type().is_some_and(|t| t.is_file())
                && let Ok(rel) = entry.path().strip_prefix(&base)
                && matcher.is_match(rel)
            {
                let full = entry.path().strip_prefix(&self.root).unwrap_or(rel);
                matches.push(full.to_string_lossy().into_owned());
            }
        }
        matches.sort();

        if matches.is_empty() {
            return Ok("(no matches)".to_string());
        }
        let mut out = matches.join("\n");
        if matches.len() >= limit {
            out.push_str(&format!("\n[truncated at {} results]", matches.len()));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp_dir() -> tempdir::TempDir {
        tempdir::TempDir::new("oven-test").unwrap()
    }

    fn write(root: &std::path::Path, rel: &str, content: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[cfg(not(target_os = "windows"))]
    #[tokio::test]
    async fn recursive_match() {
        let tmp = tmp_dir();
        let root = tmp.path();
        write(root, "src/lib.rs", "x");
        write(root, "src/main.rs", "x");
        write(root, "README.md", "x");
        let glob = GlobTool::new(root);
        let out = glob
            .run(&json!({"pattern": "**/*.rs"}), None)
            .await
            .unwrap();
        assert!(out.contains("src/lib.rs"), "{out}");
        assert!(out.contains("src/main.rs"), "{out}");
        assert!(!out.contains("README.md"), "{out}");
    }

    #[tokio::test]
    async fn respects_gitignore() {
        let tmp = tmp_dir();
        let root = tmp.path();
        write(root, "keep.txt", "x");
        write(root, ".gitignore", "skip.txt\n");
        write(root, "skip.txt", "x");
        let glob = GlobTool::new(root);
        let out = glob
            .run(&json!({"pattern": "**/*.txt"}), None)
            .await
            .unwrap();
        assert!(out.contains("keep.txt"), "{out}");
        assert!(!out.contains("skip.txt"), "{out}");
    }

    #[tokio::test]
    async fn limits_results() {
        let tmp = tmp_dir();
        let root = tmp.path();
        for i in 0..5 {
            write(root, &format!("f{i}.txt"), "x");
        }
        let glob = GlobTool::new(root);
        let out = glob
            .run(&json!({"pattern": "**/*.txt", "limit": 2}), None)
            .await
            .unwrap();
        assert!(out.contains("truncated at 2"), "{out}");
        assert_eq!(out.lines().count(), 3);
    }

    #[tokio::test]
    async fn invalid_pattern_errors() {
        let tmp = tmp_dir();
        let glob = GlobTool::new(tmp.path());
        let err = glob.run(&json!({"pattern": "["}), None).await.unwrap_err();
        assert!(err.message.contains("invalid pattern"), "{}", err.message);
    }

    #[tokio::test]
    async fn rejects_path_escape() {
        let tmp = tmp_dir();
        let glob = GlobTool::new(tmp.path());
        let err = glob
            .run(&json!({"pattern": "*.rs", "path": "../etc"}), None)
            .await
            .unwrap_err();
        assert!(err.message.contains("escapes root"), "{}", err.message);
    }
}

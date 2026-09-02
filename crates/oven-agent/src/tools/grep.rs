use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, require_str, resolve_within};
use crate::error::AgentError;
use crate::matching::{GlobMatcher, Regex, compile_glob, compile_regex};
use oven_host::walk_dir;

pub struct GrepTool {
    root: PathBuf,
    max_results: usize,
    max_line_len: usize,
}

impl GrepTool {
    pub const NAME: &'static str = "grep";

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_results: 200,
            max_line_len: 200,
        }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn description(&self) -> &str {
        "Search file contents with a regex. Returns matching lines as \
         path:line:content (paths relative to the workspace root). Respects \
         .gitignore; skips binary files."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for." },
                "path": { "type": "string", "description": "File or directory to search, relative to the workspace root. Defaults to root." },
                "include": { "type": "string", "description": "Glob filter on file names, e.g. \"*.rs\"." },
                "case_insensitive": { "type": "boolean", "description": "Match ignoring case when true." },
                "limit": { "type": "integer", "description": "Maximum number of matching lines to return. Defaults to 200." }
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
        let re = compile_regex(
            pattern,
            args.get("case_insensitive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        )
        .map_err(|e| AgentError::from(format!("grep: invalid regex {:?}: {}", pattern, e)))?;

        let include = args
            .get("include")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|p| {
                compile_glob(p).map_err(|e| {
                    AgentError::from(format!("grep: invalid include pattern {:?}: {}", p, e))
                })
            })
            .transpose()?;

        let base_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(".");
        let base = resolve_within(&self.root, base_str)?;
        if !base.exists() {
            return Err(AgentError::from(format!(
                "grep: not found: {}",
                base.display()
            )));
        }
        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .map(|v| v.max(0) as usize)
            .unwrap_or(self.max_results);

        let mut out = Vec::new();
        if base.is_file() {
            let rel = base.strip_prefix(&self.root).unwrap_or(&base);
            self.grep_file(&base, rel, &re, include.as_ref(), &mut out, limit)?;
        } else {
            for entry in walk_dir(&base) {
                if out.len() >= limit {
                    break;
                }
                if let Some(c) = cancel
                    && c.is_cancelled()
                {
                    return Err(AgentError::cancelled());
                }
                let entry = entry.map_err(|e| AgentError::from(format!("grep: walk: {}", e)))?;
                if entry.is_file() {
                    let full = entry.path();
                    let rel = full.strip_prefix(&self.root).unwrap_or(full);
                    self.grep_file(full, rel, &re, include.as_ref(), &mut out, limit)?;
                }
            }
        }

        if out.is_empty() {
            return Ok("(no matches)".to_string());
        }
        let mut text = out.join("\n");
        if out.len() >= limit {
            text.push_str(&format!("\n[truncated at {} results]", out.len()));
        }
        Ok(text)
    }
}

impl GrepTool {
    fn grep_file(
        &self,
        full: &Path,
        rel: &Path,
        re: &Regex,
        include: Option<&GlobMatcher>,
        out: &mut Vec<String>,
        limit: usize,
    ) -> Result<(), AgentError> {
        if let Some(m) = include
            && let Some(name) = full.file_name()
            && !m.is_match(Path::new(name))
        {
            return Ok(());
        }
        let bytes = std::fs::read(full)
            .map_err(|e| AgentError::from(format!("grep: read {}: {}", full.display(), e)))?;
        if bytes.contains(&0) {
            return Ok(());
        }
        let content = String::from_utf8_lossy(&bytes);
        for (idx, line) in content.lines().enumerate() {
            if out.len() >= limit {
                return Ok(());
            }
            if re.is_match(line) {
                let trimmed = if line.len() > self.max_line_len {
                    let end = line.floor_char_boundary(self.max_line_len);
                    format!("{}…", &line[..end])
                } else {
                    line.to_string()
                };
                out.push(format!("{}:{}:{}", rel.display(), idx + 1, trimmed));
            }
        }
        Ok(())
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

    #[tokio::test]
    async fn finds_matching_lines() {
        let tmp = tmp_dir();
        let root = tmp.path();
        write(root, "a.rs", "fn foo() {}\nfn bar() {}\n");
        let grep = GrepTool::new(root);
        let out = grep.run(&json!({"pattern": "foo"}), None).await.unwrap();
        assert_eq!(out, "a.rs:1:fn foo() {}");
    }

    #[tokio::test]
    async fn case_insensitive_flag() {
        let tmp = tmp_dir();
        let root = tmp.path();
        write(root, "a.txt", "Hello\nworld\n");
        let grep = GrepTool::new(root);
        let out = grep
            .run(&json!({"pattern": "hello", "case_insensitive": true}), None)
            .await
            .unwrap();
        assert_eq!(out, "a.txt:1:Hello");
        let err = grep.run(&json!({"pattern": "hello"}), None).await.unwrap();
        assert_eq!(err, "(no matches)");
    }

    #[tokio::test]
    async fn include_filters_files() {
        let tmp = tmp_dir();
        let root = tmp.path();
        write(root, "a.rs", "needle\n");
        write(root, "a.txt", "needle\n");
        let grep = GrepTool::new(root);
        let out = grep
            .run(&json!({"pattern": "needle", "include": "*.rs"}), None)
            .await
            .unwrap();
        assert_eq!(out, "a.rs:1:needle");
    }

    #[tokio::test]
    async fn skips_binary_and_gitignored() {
        let tmp = tmp_dir();
        let root = tmp.path();
        write(root, ".gitignore", "ignored.txt\n");
        write(root, "ignored.txt", "needle\n");
        std::fs::write(root.join("bin.dat"), b"needle\x00\xff").unwrap();
        write(root, "real.txt", "needle\n");
        write(root, ".github/ci.yml", "needle\n");
        let grep = GrepTool::new(root);
        let out = grep.run(&json!({"pattern": "needle"}), None).await.unwrap();
        let lines: Vec<_> = out.lines().collect();
        assert!(lines.contains(&"real.txt:1:needle"), "{out}");
        #[cfg(windows)]
        assert!(lines.contains(&r".github\ci.yml:1:needle"), "{out}");
        #[cfg(not(windows))]
        assert!(lines.contains(&".github/ci.yml:1:needle"), "{out}");
        assert!(!out.contains("ignored.txt"), "{out}");
        assert_eq!(lines.len(), 2, "{out}");
    }

    #[tokio::test]
    async fn skips_dot_dirs_without_gitignore() {
        let tmp = tmp_dir();
        let root = tmp.path();
        write(root, "real.txt", "needle\n");
        write(root, ".hidden/x.txt", "needle\n");
        let grep = GrepTool::new(root);
        let out = grep.run(&json!({"pattern": "needle"}), None).await.unwrap();
        assert_eq!(out, "real.txt:1:needle");
    }

    #[tokio::test]
    async fn invalid_regex_errors() {
        let tmp = tmp_dir();
        let grep = GrepTool::new(tmp.path());
        let err = grep.run(&json!({"pattern": "("}), None).await.unwrap_err();
        assert!(err.message.contains("invalid regex"), "{}", err.message);
    }

    #[tokio::test]
    async fn rejects_path_escape() {
        let tmp = tmp_dir();
        let grep = GrepTool::new(tmp.path());
        let err = grep
            .run(&json!({"pattern": "x", "path": "../etc"}), None)
            .await
            .unwrap_err();
        assert!(err.message.contains("escapes root"), "{}", err.message);
    }
}

//! Instruction file discovery and loading.
//!
//! `AGENTS.md` / `CLAUDE.md` files in the user config dir and the workspace
//! root are loaded once at config time and injected into the system prompt.

use std::fmt;
use std::path::{Path, PathBuf};

/// Where an instruction document was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionScope {
    Global,
    Project,
}

impl fmt::Display for InstructionScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Global => write!(f, "Global"),
            Self::Project => write!(f, "Project"),
        }
    }
}

/// A loaded instruction document.
#[derive(Debug, Clone)]
pub struct InstructionDoc {
    pub scope: InstructionScope,
    pub path: PathBuf,
    pub content: String,
}

/// Load every existing, non-empty instruction document in discovery order:
/// user config dir first, then the workspace root; within a dir `AGENTS.md`
/// before `CLAUDE.md`. Missing or unreadable files are skipped silently.
pub(crate) fn load_instructions(config_home: Option<&Path>, root: &Path) -> Vec<InstructionDoc> {
    let mut docs = Vec::new();
    if let Some(home) = config_home {
        docs.extend(load_from_dir(home, InstructionScope::Global));
    }
    docs.extend(load_from_dir(root, InstructionScope::Project));
    docs
}

fn load_from_dir(dir: &Path, scope: InstructionScope) -> Vec<InstructionDoc> {
    ["AGENTS.md", "CLAUDE.md"]
        .into_iter()
        .filter_map(|name| {
            let path = dir.join(name);
            if !path.is_file() {
                return None;
            }
            let content = std::fs::read_to_string(&path).ok()?;
            if content.trim().is_empty() {
                return None;
            }
            Some(InstructionDoc {
                scope,
                path,
                content,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_files_return_empty() {
        let tmp = tempdir::TempDir::new("inst-none").unwrap();
        let docs = load_instructions(Some(tmp.path()), tmp.path());
        assert!(docs.is_empty());
    }

    #[test]
    fn loads_both_names_and_dirs_in_order() {
        let tmp = tempdir::TempDir::new("inst-order").unwrap();
        let home = tmp.path().join("config");
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(home.join("AGENTS.md"), "user agents\n").unwrap();
        std::fs::write(home.join("CLAUDE.md"), "user claude\n").unwrap();
        std::fs::write(root.join("AGENTS.md"), "proj agents\n").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "proj claude\n").unwrap();

        let docs = load_instructions(Some(&home), &root);
        let contents: Vec<&str> = docs.iter().map(|d| d.content.as_str()).collect();
        assert_eq!(
            contents,
            [
                "user agents\n",
                "user claude\n",
                "proj agents\n",
                "proj claude\n"
            ]
        );
        assert_eq!(docs[0].scope, InstructionScope::Global);
        assert_eq!(docs[2].scope, InstructionScope::Project);
        assert_eq!(docs[2].path, root.join("AGENTS.md"));
    }

    #[test]
    fn claude_fallback_and_empty_file_skipped() {
        let tmp = tempdir::TempDir::new("inst-claude").unwrap();
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "rules\n").unwrap();
        std::fs::write(root.join("AGENTS.md"), "   \n").unwrap();

        let docs = load_instructions(None, &root);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].content, "rules\n");
    }

    #[test]
    fn unreadable_file_is_skipped() {
        let tmp = tempdir::TempDir::new("inst-unreadable").unwrap();
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        // A directory named AGENTS.md is not readable as a file.
        std::fs::create_dir_all(root.join("AGENTS.md")).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "rules\n").unwrap();

        let docs = load_instructions(None, &root);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].content, "rules\n");
    }
}

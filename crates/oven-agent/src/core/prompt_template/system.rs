use std::fmt::Write;
use std::path::Path;

use super::instructions::InstructionDoc;

const BASE_PROMPT: &str = include_str!("system_prompt.md");
pub const MEMORY_PROMPT: &str = include_str!("memory.md");

pub fn system_prompt(
    root: &Path,
    instructions: &[InstructionDoc],
    skills: Option<String>,
) -> String {
    let mut out = BASE_PROMPT.trim().to_string();
    out.push_str("\n\n");
    out.push_str(&env_block(root));
    for doc in instructions {
        let _ = write!(
            out,
            "\n\n## {} Instructions (from {})\n\n{}\n\n",
            doc.scope,
            doc.path.display(),
            doc.content
        );
    }
    if let Some(extra) = skills {
        out.push_str("\n\n## Available Skills\n\n");
        out.push_str(&extra);
    }
    out
}

fn env_block(root: &Path) -> String {
    let workspace = canonical_root(root);
    let git = if is_git_repo(root) { "yes" } else { "no" };
    format!(
        "<env>\n  Workspace root folder: {workspace}\n  Is directory a git repo: {git}\n  Platform: {}\n  Today's date: {}\n</env>",
        platform(),
        today(),
    )
}

fn canonical_root(root: &Path) -> String {
    root.canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[inline]
fn platform() -> &'static str {
    std::env::consts::OS
}

#[inline]
fn is_git_repo(start: &Path) -> bool {
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    start.ancestors().any(|dir| dir.join(".git").exists())
}

#[inline]
fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d %:z").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::core::prompt_template::instructions::{InstructionScope, load_instructions};

    #[test]
    fn system_prompt_includes_bundled_markdown() {
        let prompt = system_prompt(Path::new("."), &[], None);
        assert!(prompt.starts_with(BASE_PROMPT.trim()));
    }

    #[test]
    fn shared_prompt_leaves_top_level_tools_to_the_caller() {
        for tool in ["`answer`", "`todo_write`", "`task`"] {
            assert!(!BASE_PROMPT.contains(tool), "{tool}");
        }
    }

    #[tokio::test]
    async fn system_prompt_includes_instruction_docs() {
        let tmp = tempdir::TempDir::new("app-instructions").unwrap();
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "project rules\n").unwrap();

        let docs = load_instructions(None, &root).await;
        let prompt = system_prompt(&root, &docs, None);
        assert!(prompt.contains("## Project Instructions"));
        assert!(prompt.contains("project rules"));
        assert!(prompt.contains("<env>"));
        assert!(prompt.find("</env>").unwrap() < prompt.find("## Project Instructions").unwrap());
    }

    #[test]
    fn system_prompt_labels_user_and_project_docs() {
        let docs = vec![
            InstructionDoc {
                scope: InstructionScope::Global,
                path: PathBuf::from("/cfg/AGENTS.md"),
                content: "global rules\n".into(),
            },
            InstructionDoc {
                scope: InstructionScope::Project,
                path: PathBuf::from("/ws/CLAUDE.md"),
                content: "project rules\n".into(),
            },
        ];
        let prompt = system_prompt(Path::new("."), &docs, None);
        assert!(prompt.contains("## Global Instructions (from /cfg/AGENTS.md)"));
        assert!(prompt.contains("## Project Instructions (from /ws/CLAUDE.md)"));
        assert!(prompt.find("global rules").unwrap() < prompt.find("project rules").unwrap());
    }

    #[test]
    fn system_prompt_appends_skills() {
        let prompt = system_prompt(Path::new("."), &[], Some("- **files**: read files".into()));
        assert!(prompt.contains("## Available Skills"));
        assert!(prompt.contains("- **files**: read files"));
    }

    #[test]
    fn env_block_lists_workspace_and_git() {
        let tmp = tempdir::TempDir::new("env-git").unwrap();
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(root.join(".git")).unwrap();

        let block = env_block(&root);
        let workspace = canonical_root(&root);
        assert!(block.starts_with("<env>\n"));
        assert!(block.contains(&format!("  Workspace root folder: {workspace}")));
        assert!(block.contains("  Is directory a git repo: yes"));
        assert!(block.contains(&format!("  Platform: {}", platform())));
        assert!(block.contains(&format!("  Today's date: {}", today())));
        assert!(!block.contains("Working directory"));
        assert!(block.ends_with("</env>"));
    }

    #[test]
    fn env_block_detects_a_git_repo_in_a_parent() {
        let tmp = tempdir::TempDir::new("env-parent-git").unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        let nested = tmp.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let block = env_block(&nested);
        assert!(block.contains("  Is directory a git repo: yes"));
    }

    #[test]
    fn env_block_reports_non_git() {
        let tmp = tempdir::TempDir::new("env-nogit").unwrap();
        let block = env_block(tmp.path());
        assert!(block.contains("  Is directory a git repo: no"));
    }
}

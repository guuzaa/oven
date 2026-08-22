use std::path::Path;

use crate::instructions::InstructionDoc;
use crate::session::canonical_root;

const BASE_PROMPT: &str = include_str!("system_prompt.md");

pub(crate) fn build_system_prompt(
    root: &Path,
    instructions: &[InstructionDoc],
    skills: Option<String>,
) -> String {
    let mut out = BASE_PROMPT.trim().to_string();
    out.push_str("\n\n");
    out.push_str(&env_block(root));
    for doc in instructions {
        out.push_str(&format!(
            "\n\n## {} Instructions (from {})\n\n{}\n\n",
            doc.scope,
            doc.path.display(),
            doc.content
        ));
    }
    if let Some(extra) = skills {
        out.push_str("\n\n## Available Skills\n\n");
        out.push_str(&extra);
    }
    out
}

pub(crate) fn env_block(root: &Path) -> String {
    let workspace = canonical_root(root);
    let cwd = std::env::current_dir()
        .map(|p| canonical_root(&p))
        .unwrap_or_else(|_| workspace.clone());
    let git = if is_git_repo(root) { "yes" } else { "no" };
    format!(
        "<env>\n  Working directory: {cwd}\n  Workspace root folder: {workspace}\n  Is directory a git repo: {git}\n  Platform: {}\n  Today's date: {}\n</env>",
        platform(),
        today(),
    )
}

#[inline]
fn platform() -> &'static str {
    std::env::consts::OS
}

fn is_git_repo(start: &Path) -> bool {
    let mut dir = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    loop {
        if dir.join(".git").exists() {
            return true;
        }
        if !dir.pop() {
            return false;
        }
    }
}

#[inline]
fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::instructions::{InstructionScope, load_instructions};

    #[test]
    fn system_prompt_includes_bundled_markdown() {
        let prompt = build_system_prompt(Path::new("."), &[], None);
        assert!(prompt.starts_with(BASE_PROMPT.trim()));
    }

    #[test]
    fn system_prompt_includes_instruction_docs() {
        let tmp = tempdir::TempDir::new("app-instructions").unwrap();
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "project rules\n").unwrap();

        let docs = load_instructions(None, &root);
        let prompt = build_system_prompt(&root, &docs, None);
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
        let prompt = build_system_prompt(Path::new("."), &docs, None);
        assert!(prompt.contains("## Global Instructions (from /cfg/AGENTS.md)"));
        assert!(prompt.contains("## Project Instructions (from /ws/CLAUDE.md)"));
        assert!(prompt.find("global rules").unwrap() < prompt.find("project rules").unwrap());
    }

    #[test]
    fn system_prompt_appends_skills() {
        let prompt =
            build_system_prompt(Path::new("."), &[], Some("- **files**: read files".into()));
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
        assert!(block.ends_with("</env>"));
    }

    #[test]
    fn env_block_reports_non_git() {
        let tmp = tempdir::TempDir::new("env-nogit").unwrap();
        let block = env_block(tmp.path());
        assert!(block.contains("  Is directory a git repo: no"));
    }
}

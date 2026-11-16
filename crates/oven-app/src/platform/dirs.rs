use std::path::{Path, PathBuf};

use oven_mem::{MemoryRoots, project_key};

const OVEN_DIR: &str = ".oven";
const MEMORY_DIR: &str = "memory";
const PROJECTS_DIR: &str = "projects";
const INBOX_FILE: &str = "inbox.jsonl";

fn oven_home() -> Option<PathBuf> {
    std::env::home_dir().map(|h| h.join(OVEN_DIR))
}

/// Absolute, symlink-resolved form of a workspace root, falling back to the
/// raw path when canonicalization fails. Used as the key for the session
/// meta record, the `cwd_latest.json` index and the project memory layer,
/// so all three agree.
#[inline]
pub fn canonical_root(root: &Path) -> String {
    root.canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// `~/.oven`.
#[inline]
pub fn config_home() -> Option<PathBuf> {
    oven_home()
}

/// `~/.oven/config.toml`.
#[inline]
pub fn user_config_path() -> Option<PathBuf> {
    oven_home().map(|d| d.join("config.toml"))
}

/// `~/.oven/sessions`.
#[inline]
pub fn sessions_dir() -> Option<PathBuf> {
    oven_home().map(|d| d.join("sessions"))
}

/// `~/.oven/logs`.
#[inline]
pub fn logs_dir() -> Option<PathBuf> {
    oven_home().map(|d| d.join("logs"))
}

/// `~/.oven/projects`: one directory per workspace root.
#[inline]
pub fn projects_dir() -> Option<PathBuf> {
    oven_home().map(|d| d.join(PROJECTS_DIR))
}

/// `~/.oven/projects/<key>` for a workspace root.
pub fn project_dir(root: &Path) -> Option<PathBuf> {
    projects_dir().map(|d| d.join(project_key(&canonical_root(root))))
}

/// The memory layers of a workspace: `project` and `global` under the home
/// directory, `shared` in the repository.
pub fn memory_roots(root: &Path) -> MemoryRoots {
    MemoryRoots {
        workspace: root.to_path_buf(),
        project: project_dir(root).map(|d| d.join(MEMORY_DIR)),
        shared: root.join(OVEN_DIR).join(MEMORY_DIR),
        global: oven_home().map(|home| home.join(MEMORY_DIR)),
        inbox: project_dir(root).map(|d| d.join(INBOX_FILE)),
    }
}

/// Skill search paths: user-wide `~/.oven/skills` first, then the project's `.oven/skills`.
/// Later paths override earlier skills with the same id.
pub fn skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity(2);
    if let Some(d) = oven_home().map(|d| d.join("skills")) {
        dirs.push(d);
    }
    dirs.push(root.join(OVEN_DIR).join("skills"));
    dirs
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use oven_mem::project_key;

    use super::{canonical_root, memory_roots};

    #[test]
    fn memory_layers_split_between_home_and_repository() {
        let root = Path::new("/workspace");
        let roots = memory_roots(root);
        assert_eq!(roots.workspace, root);
        assert_eq!(roots.shared, root.join(".oven").join("memory"));
        let Some(home) = std::env::home_dir() else {
            assert!(roots.project.is_none());
            assert!(roots.global.is_none());
            assert!(roots.inbox.is_none());
            return;
        };
        let project = home
            .join(".oven")
            .join("projects")
            .join(project_key(&canonical_root(root)));
        assert_eq!(roots.global, Some(home.join(".oven").join("memory")));
        assert_eq!(roots.project, Some(project.join("memory")));
        assert_eq!(roots.inbox, Some(project.join("inbox.jsonl")));
    }
}

use std::path::{Path, PathBuf};

fn oven_home() -> Option<PathBuf> {
    std::env::home_dir().map(|h| h.join(".oven"))
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

/// Workspace `<root>/.oven/memory` and, when a home directory exists, `~/.oven/memory`.
pub fn memory_roots(root: &Path) -> oven_mem::MemoryRoots {
    oven_mem::MemoryRoots {
        workspace: root.join(".oven").join("memory"),
        user: oven_home().map(|home| home.join("memory")),
    }
}

/// Skill search paths: user-wide `~/.oven/skills` first, then the project's `.oven/skills`.
/// Later paths override earlier skills with the same id.
pub fn skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity(2);
    if let Some(d) = oven_home().map(|d| d.join("skills")) {
        dirs.push(d);
    }
    dirs.push(root.join(".oven").join("skills"));
    dirs
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::memory_roots;

    #[test]
    fn memory_roots_place_workspace_under_oven_and_user_under_home() {
        let root = Path::new("/workspace");
        let roots = memory_roots(root);
        assert_eq!(roots.workspace, root.join(".oven").join("memory"));
        if let Some(home) = std::env::home_dir() {
            assert_eq!(roots.user, Some(home.join(".oven").join("memory")));
        } else {
            assert!(roots.user.is_none());
        }
    }
}

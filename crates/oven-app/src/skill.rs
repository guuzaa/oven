//! Skill search paths.
//!
//! The skill domain (trait, registry, dynamic content loading, `skill_read`
//! tool) lives in [`oven_agent`]; this module only resolves where skill
//! directories are searched: the user data dir and the project's
//! `.oven/skills`.

use std::path::{Path, PathBuf};

/// Default user-level skills directory: `$XDG_DATA_HOME/oven/skills` (or
/// `~/.local/share/oven/skills`).
pub fn default_skills_dir() -> Option<PathBuf> {
    cross_xdg::BaseDirs::with_prefix("oven")
        .ok()
        .map(|d| d.data_home().join("skills"))
}

/// Skill search paths: user-wide first, then the project's `.oven/skills`.
/// Later paths override earlier skills with the same id.
pub fn skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity(2);
    if let Some(d) = default_skills_dir() {
        dirs.push(d);
    }
    dirs.push(root.join(".oven").join("skills"));
    dirs
}

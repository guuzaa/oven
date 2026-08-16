//! Skill search paths.
//!
//! The skill domain (trait, registry, dynamic content loading, `skill_read`
//! tool) lives in [`oven_agent`]; this module only resolves where skill
//! directories are searched: the user data dir and the project's
//! `.oven/skills`.

use std::path::{Path, PathBuf};

/// Skill search paths: user-wide first, then the project's `.oven/skills`.
/// Later paths override earlier skills with the same id.
pub fn skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity(2);
    if let Some(d) = crate::dirs::skills_dir() {
        dirs.push(d);
    }
    dirs.push(root.join(".oven").join("skills"));
    dirs
}

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

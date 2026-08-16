use std::path::PathBuf;

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

/// `~/.oven/skills`.
#[inline]
pub fn skills_dir() -> Option<PathBuf> {
    oven_home().map(|d| d.join("skills"))
}

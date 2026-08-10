//! Skills: named guidance modules that contribute system context to the
//! agent.
//!
//! Each skill is a directory containing a `SKILL.md` file with a
//! `description:` YAML frontmatter. Only the description is injected into
//! the system prompt (as `- **<id>**: <description>` lines); the full document
//! body is never loaded up front. Instead it is read from disk on demand
//! via [`SkillRegistry::content`], which backs the
//! [`SkillReadTool`](crate::tools::SkillReadTool).
//!
//! Discovery is directory-driven: [`SkillRegistry::load_from_dirs`] scans
//! each directory's immediate subdirectories. The app layer decides which
//! directories to search (user data dir, project `.oven/skills`, ...) and
//! later dirs override earlier skills with the same id.
//!
//! A skill is deliberately *not* a tool bundle: it only contributes
//! guidance. Tools are a separate concern, mounted independently.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Canonical filename of the guidance document inside a skill directory.
pub(crate) const SKILL_FILE: &str = "SKILL.md";

/// A guidance module. Skills are discovered once at startup and their
/// descriptions are merged into the [`Agent`](crate::Agent) system prompt.
pub trait Skill: Send + Sync {
    /// Stable identifier, e.g. `"files"`. Matches the skill directory name.
    fn id(&self) -> &str;
    /// Short description injected into the system prompt.
    fn description(&self) -> &str;
    /// Source document on disk. When present, [`SkillRegistry::content`] can
    /// load the full guidance dynamically; otherwise the skill has no body.
    fn source(&self) -> Option<&Path> {
        None
    }
}

/// Collects discovered skills and exposes their merged prompt contribution
/// in one place.
#[derive(Default)]
pub struct SkillRegistry {
    skills: BTreeMap<String, Box<dyn Skill>>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, skill: Box<dyn Skill>) {
        let id = skill.id().to_string();
        self.skills.insert(id, skill);
    }

    pub fn contains(&self, id: &str) -> bool {
        self.skills.contains_key(id)
    }

    pub fn ids(&self) -> Vec<&str> {
        self.skills.keys().map(|s| s.as_str()).collect()
    }

    /// (id, source path) pairs for skills backed by a document on disk.
    pub fn sources(&self) -> Vec<(String, PathBuf)> {
        self.skills
            .iter()
            .filter_map(|(id, s)| s.source().map(|p| (id.clone(), p.to_path_buf())))
            .collect()
    }

    /// System prompt contribution: one `- **<id>**: <description>` line per
    /// skill. The order is lexicographic (deterministic across runs).
    pub fn merged_system_prompt(&self) -> Option<String> {
        let mut parts = Vec::with_capacity(self.skills.len());
        for (id, skill) in &self.skills {
            parts.push(format!("- **{id}**: {}", skill.description()));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }

    /// Dynamically load the full guidance document for a skill from disk.
    /// Content is read on every call, so edits take effect immediately.
    pub fn content(&self, id: &str) -> Option<String> {
        let path = self.skills.get(id)?.source()?;
        std::fs::read_to_string(path).ok()
    }

    /// Discover skills from the given directories. For each immediate
    /// subdirectory containing a `SKILL.md` file with a `description:`
    /// frontmatter, the directory name becomes the skill id. Later
    /// directories override earlier ones; missing or unreadable entries are
    /// skipped.
    pub fn load_from_dirs(&mut self, dirs: &[PathBuf]) {
        for dir in dirs {
            self.load_from_dir(dir);
        }
    }

    fn load_from_dir(&mut self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let dir_path = entry.path();
            if !dir_path.is_dir() {
                continue;
            }
            let Some(id) = dir_path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(file) = find_skill_file(&dir_path) else {
                continue;
            };
            let Ok(raw) = std::fs::read_to_string(&file) else {
                continue;
            };
            let Some(description) = parse_frontmatter(&raw) else {
                continue;
            };
            self.register(Box::new(FileSkill {
                id: id.to_string(),
                description,
                path: file,
            }));
        }
    }
}

struct FileSkill {
    id: String,
    description: String,
    path: PathBuf,
}

impl Skill for FileSkill {
    fn id(&self) -> &str {
        &self.id
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn source(&self) -> Option<&Path> {
        Some(&self.path)
    }
}

fn find_skill_file(dir: &Path) -> Option<PathBuf> {
    [SKILL_FILE]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())
}

/// Parse the YAML frontmatter between `---` fences and return `description`.
fn parse_frontmatter(raw: &str) -> Option<String> {
    let rest = raw.trim_start().strip_prefix("---")?;
    let (front, _) = rest.split_once("---")?;
    let meta: serde_yaml::Value = serde_yaml::from_str(front).ok()?;
    let desc = meta.get("description")?.as_str()?.trim().to_string();
    if desc.is_empty() { None } else { Some(desc) }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct HelperSkill;
    impl Skill for HelperSkill {
        fn id(&self) -> &str {
            "helper"
        }
        fn description(&self) -> &str {
            "be helpful"
        }
    }

    #[test]
    fn merged_system_prompt_uses_id_and_description() {
        let mut reg = SkillRegistry::new();
        reg.register(Box::new(HelperSkill));
        let p = reg.merged_system_prompt().unwrap();
        assert!(p.contains("- **helper**: be helpful"));
    }

    #[test]
    fn empty_registry_contributes_nothing() {
        assert!(SkillRegistry::new().merged_system_prompt().is_none());
    }

    #[test]
    fn loads_skills_from_directories() {
        let tmp = tempdir::TempDir::new("skill-fs").unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("files")).unwrap();
        std::fs::write(
            dir.join("files").join(SKILL_FILE),
            "---\ndescription: read files carefully\n---\nfull guidance\n",
        )
        .unwrap();

        let mut reg = SkillRegistry::new();
        reg.load_from_dirs(&[dir.to_path_buf()]);
        assert!(reg.contains("files"));
        let p = reg.merged_system_prompt().unwrap();
        assert!(p.contains("- **files**: read files carefully"));
        assert!(!p.contains("full guidance"));
        assert_eq!(
            reg.content("files").unwrap(),
            "---\ndescription: read files carefully\n---\nfull guidance\n"
        );
    }

    #[test]
    fn later_dirs_override_same_skill_id() {
        let tmp = tempdir::TempDir::new("skill-override").unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        for dir in [&a, &b] {
            std::fs::create_dir_all(dir.join("s")).unwrap();
        }
        std::fs::write(
            a.join("s").join(SKILL_FILE),
            "---\ndescription: first\n---\nbody a\n",
        )
        .unwrap();
        std::fs::write(
            b.join("s").join(SKILL_FILE),
            "---\ndescription: second\n---\nbody b\n",
        )
        .unwrap();

        let mut reg = SkillRegistry::new();
        reg.load_from_dirs(&[a, b]);
        let p = reg.merged_system_prompt().unwrap();
        assert!(p.contains("second"));
        assert!(!p.contains("first"));
    }

    #[test]
    fn skills_without_description_are_skipped() {
        let tmp = tempdir::TempDir::new("skill-nodesc").unwrap();
        std::fs::create_dir_all(tmp.path().join("x")).unwrap();
        std::fs::write(tmp.path().join("x").join(SKILL_FILE), "no frontmatter here").unwrap();

        let mut reg = SkillRegistry::new();
        reg.load_from_dirs(&[tmp.path().to_path_buf()]);
        assert!(!reg.contains("x"));
    }

    #[test]
    fn content_is_loaded_dynamically() {
        let tmp = tempdir::TempDir::new("skill-dynamic").unwrap();
        std::fs::create_dir_all(tmp.path().join("s")).unwrap();
        let file = tmp.path().join("s").join(SKILL_FILE);
        std::fs::write(&file, "---\ndescription: d\n---\nbody 1\n").unwrap();

        let mut reg = SkillRegistry::new();
        reg.load_from_dirs(&[tmp.path().to_path_buf()]);
        assert_eq!(
            reg.content("s").unwrap(),
            "---\ndescription: d\n---\nbody 1\n"
        );

        std::fs::write(&file, "---\ndescription: d\n---\nbody 2\n").unwrap();
        assert_eq!(
            reg.content("s").unwrap(),
            "---\ndescription: d\n---\nbody 2\n"
        );
    }
}

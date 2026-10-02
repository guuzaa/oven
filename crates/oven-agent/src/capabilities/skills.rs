//! Skills: named guidance modules that contribute system context to the
//! agent.
//!
//! Each skill is a directory containing a `SKILL.md` file with a
//! `description:` YAML frontmatter. Only the description is injected into
//! the system prompt (as `- **<id>**: <description>` lines); the full document
//! body is never loaded up front. [`SkillReadTool`](crate::capabilities::tools::SkillReadTool)
//! reads it from the paths reported by [`SkillRegistry::sources`].
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
use std::string::String;

use oven_host::split_frontmatter;
use tokio::fs;

/// Canonical filename of the guidance document inside a skill directory.
pub(crate) const SKILL_FILE: &str = "SKILL.md";

/// A guidance module. Skills are discovered once at startup and their
/// descriptions are merged into the [`Agent`](crate::Agent) system prompt.
pub trait Skill: Send + Sync {
    /// Stable identifier, e.g. `"files"`. Matches the skill directory name.
    fn id(&self) -> &str;
    /// Short description injected into the system prompt.
    fn description(&self) -> &str;
    /// Source document on disk. When present, the skill body can be read on
    /// demand; otherwise the skill has no body.
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
        self.skills.keys().map(String::as_str).collect()
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

    /// Discover skills from the given directories. For each immediate
    /// subdirectory containing a `SKILL.md` file with a `description:`
    /// frontmatter, the directory name becomes the skill id. Later
    /// directories override earlier ones; missing or unreadable entries are
    /// skipped.
    pub async fn load_from_dirs(&mut self, dirs: &[PathBuf]) {
        for dir in dirs {
            self.load_from_dir(dir).await;
        }
    }

    async fn load_from_dir(&mut self, dir: &Path) {
        let mut entries = match fs::read_dir(dir).await {
            Ok(entries) => entries,
            Err(_) => return,
        };
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(_) => continue,
            };
            let dir_path = entry.path();
            let Ok(meta) = fs::metadata(&dir_path).await else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            let Some(id) = dir_path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(file) = find_skill_file(&dir_path).await else {
                continue;
            };
            let Ok(raw) = fs::read_to_string(&file).await else {
                continue;
            };
            let Some(description) = parse_description(&raw) else {
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

async fn find_skill_file(dir: &Path) -> Option<PathBuf> {
    for name in [SKILL_FILE] {
        let path = dir.join(name);
        if let Ok(meta) = fs::metadata(&path).await
            && meta.is_file()
        {
            return Some(path);
        }
    }
    None
}

fn parse_description(raw: &str) -> Option<String> {
    let (front, _) = split_frontmatter(raw)?;
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

    #[tokio::test]
    async fn loads_skills_from_directories() {
        let tmp = tempdir::TempDir::new("skill-fs").unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("files")).unwrap();
        std::fs::write(
            dir.join("files").join(SKILL_FILE),
            "---\ndescription: read files carefully\n---\nfull guidance\n",
        )
        .unwrap();

        let mut reg = SkillRegistry::new();
        reg.load_from_dirs(&[dir.to_path_buf()]).await;
        assert!(reg.contains("files"));
        let p = reg.merged_system_prompt().unwrap();
        assert!(p.contains("- **files**: read files carefully"));
        assert!(!p.contains("full guidance"));
    }

    #[tokio::test]
    async fn later_dirs_override_same_skill_id() {
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
        reg.load_from_dirs(&[a, b]).await;
        let p = reg.merged_system_prompt().unwrap();
        assert!(p.contains("second"));
        assert!(!p.contains("first"));
    }

    #[test]
    fn description_containing_dashes_is_kept_whole() {
        assert_eq!(
            parse_description("---\ndescription: HTTP/2 only --- no fallback\n---\nbody\n")
                .as_deref(),
            Some("HTTP/2 only --- no fallback")
        );
    }

    #[tokio::test]
    async fn skills_without_description_are_skipped() {
        let tmp = tempdir::TempDir::new("skill-nodesc").unwrap();
        std::fs::create_dir_all(tmp.path().join("x")).unwrap();
        std::fs::write(tmp.path().join("x").join(SKILL_FILE), "no frontmatter here").unwrap();

        let mut reg = SkillRegistry::new();
        reg.load_from_dirs(&[tmp.path().to_path_buf()]).await;
        assert!(!reg.contains("x"));
    }
}

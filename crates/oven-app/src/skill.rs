//! Skills: named capabilities that contribute system context, tools, and
//! slash commands to the agent at session start.
//!
//! A "skill" is intentionally broad: it can inject guidance into the system
//! prompt, register tools, and add slash commands. The `SkillRegistry`
//! collects skills the user opted into and exposes everything the agent needs
//! in one go.
//!
//! The bundled skills live the App layer (next to `FileReadTool` etc.) — they
//! are how declarative features like the "files" or "git" skill are wired in
//! without touching the Agent crate.

use std::collections::BTreeMap;

use oven_agent::Tool;

/// A capability module. Skills are registered once at app startup and their
/// contributions are merged into the running [`oven_agent::Agent`].
pub trait Skill: Send + Sync {
    /// Stable identifier, e.g. `"files"`. Matches config keys.
    fn id(&self) -> &str;
    fn description(&self) -> &str;
    /// Additional system prompt text. `None` means "no contribution".
    fn system_prompt(&self) -> Option<String> {
        None
    }
    /// Tools this skill exposes. Empty by default.
    fn tools(&self) -> Vec<Box<dyn Tool>> {
        Vec::new()
    }
}

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

    /// Combined system prompt, with skill ids as headers. The order is
    /// lexicographic (deterministic across runs).
    pub fn merged_system_prompt(&self) -> Option<String> {
        let mut parts = Vec::new();
        for (id, skill) in &self.skills {
            if let Some(p) = skill.system_prompt() {
                parts.push(format!("[skill: {id}]\n{p}"));
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    }

    /// Concatenate tools from all registered skills.
    pub fn merged_tools(&self) -> Vec<Box<dyn Tool>> {
        let mut out = Vec::new();
        for skill in self.skills.values() {
            out.extend(skill.tools());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oven_agent::Tool as AgentTool;

    struct HelperSkill;
    impl Skill for HelperSkill {
        fn id(&self) -> &str {
            "helper"
        }
        fn description(&self) -> &str {
            "test skill"
        }
        fn system_prompt(&self) -> Option<String> {
            Some("be helpful".into())
        }
    }

    struct ToolSkill {
        root: std::path::PathBuf,
    }
    impl Skill for ToolSkill {
        fn id(&self) -> &str {
            "toolbag"
        }
        fn description(&self) -> &str {
            "adds a tool"
        }
        fn tools(&self) -> Vec<Box<dyn AgentTool>> {
            vec![Box::new(oven_agent::FileReadTool::new(self.root.clone()))]
        }
    }

    struct EmptySkill;
    impl Skill for EmptySkill {
        fn id(&self) -> &str {
            "empty"
        }
        fn description(&self) -> &str {
            ""
        }
    }

    #[test]
    fn merged_system_prompt_uses_skill_id_headers() {
        let mut reg = SkillRegistry::new();
        reg.register(Box::new(HelperSkill));
        let p = reg.merged_system_prompt().unwrap();
        assert!(p.contains("[skill: helper]"));
        assert!(p.contains("be helpful"));
    }

    #[test]
    fn registry_merges_tools_from_skills() {
        let tmp = tempdir::TempDir::new("skill-tools").unwrap();
        let mut reg = SkillRegistry::new();
        reg.register(Box::new(ToolSkill {
            root: tmp.path().to_path_buf(),
        }));
        let tools = reg.merged_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "file_read");
    }

    #[test]
    fn skills_without_contributions_contribute_nothing() {
        let mut reg = SkillRegistry::new();
        reg.register(Box::new(EmptySkill));
        assert!(reg.merged_system_prompt().is_none());
        assert!(reg.merged_tools().is_empty());
    }
}

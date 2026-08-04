//! Skills: named guidance modules that contribute system context to the agent
//! at session start.
//!
//! A skill is deliberately *not* a tool bundle: it only injects instructions
//! into the system prompt under a stable id the user opts into via the
//! `skills:` list in `config.toml`. Tools are a separate concern, mounted
//! independently through [`crate::tools::ToolRegistry`] (see the `tools:`
//! config key). Keeping the two apart means a skill is pure context while a
//! tool is an executable capability.

use std::collections::BTreeMap;

/// A guidance module. Skills are registered once at app startup and their
/// prompt contributions are merged into the running [`oven_agent::Agent`].
pub trait Skill: Send + Sync {
    /// Stable identifier, e.g. `"files"`. Matches config keys.
    fn id(&self) -> &str;
    fn description(&self) -> &str;
    /// Additional system prompt text. `None` means "no contribution".
    fn system_prompt(&self) -> Option<String> {
        None
    }
}

/// Collects skills the user opted into and exposes their merged prompt
/// contribution in one place.
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
            "test skill"
        }
        fn system_prompt(&self) -> Option<String> {
            Some("be helpful".into())
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
    fn skills_without_prompt_contribute_nothing() {
        let mut reg = SkillRegistry::new();
        reg.register(Box::new(EmptySkill));
        assert!(reg.merged_system_prompt().is_none());
    }
}

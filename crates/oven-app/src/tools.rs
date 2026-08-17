//! Bundled tools and their registry.
//!
//! Tools are executable capabilities the model can invoke (`file_read`,
//! `file_write`, `bash`). They are deliberately kept separate from
//! [`crate::skill::Skill`]: a skill contributes guidance to the system prompt,
//! while a tool is something the agent can actually call. [`ToolRegistry`]
//! owns the mounted set and resolves the user's `tools:` config list.

use std::collections::BTreeMap;
use std::path::PathBuf;

use oven_agent::{BUILTIN_TOOLS, Tool};

/// A factory producing one tool instance. Tools are rebuilt on every agent
/// spawn, so the registry hands out fresh `Box<dyn Tool>`s on demand.
type ToolFactory = Box<dyn Fn() -> Box<dyn Tool> + Send + Sync>;

/// Registry of named tools to mount on every agent.
#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, ToolFactory>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool under a stable name. Duplicate names overwrite.
    pub fn register(
        &mut self,
        name: &str,
        make: impl Fn() -> Box<dyn Tool> + Send + Sync + 'static,
    ) {
        self.tools.insert(name.to_string(), Box::new(make));
    }

    /// Resolve the configured tool set for a workspace root. An empty
    /// `requested` list mounts the built-in defaults; unknown names are
    /// silently skipped.
    pub fn from_config(root: impl Into<PathBuf>, requested: &[String]) -> Self {
        let root = root.into();
        let selected: Vec<&oven_agent::BuiltinTool> = if requested.is_empty() {
            BUILTIN_TOOLS.iter().collect()
        } else {
            BUILTIN_TOOLS
                .iter()
                .filter(|t| requested.iter().any(|n| n == t.name))
                .collect()
        };
        let mut registry = Self::new();
        for spec in selected {
            let r = root.clone();
            let make = spec.make;
            registry.register(spec.name, move || make(r.clone()));
        }
        registry
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Registered tool names, deterministically ordered.
    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    /// Fresh tool instances for one agent, in registry order.
    pub fn merged_tools(&self) -> Vec<Box<dyn Tool>> {
        self.tools.values().map(|make| make()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_request_mounts_builtin_defaults() {
        let tmp = tempdir::TempDir::new("tools-default").unwrap();
        let reg = ToolRegistry::from_config(tmp.path(), &[]);
        assert_eq!(reg.len(), BUILTIN_TOOLS.len());
        for spec in BUILTIN_TOOLS {
            assert!(reg.contains(spec.name), "{} missing", spec.name);
        }
    }

    #[test]
    fn requested_subset_is_mounted() {
        let tmp = tempdir::TempDir::new("tools-subset").unwrap();
        let reg = ToolRegistry::from_config(tmp.path(), &["bash".to_string()]);
        assert_eq!(reg.names(), vec!["bash"]);
    }

    #[test]
    fn unknown_names_are_skipped() {
        let tmp = tempdir::TempDir::new("tools-unknown").unwrap();
        let reg = ToolRegistry::from_config(
            tmp.path(),
            &["file_read".to_string(), "nope-id".to_string()],
        );
        assert!(reg.contains("file_read"));
        assert!(!reg.contains("nope-id"));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn merged_tools_builds_fresh_instances() {
        let tmp = tempdir::TempDir::new("tools-merged").unwrap();
        let reg = ToolRegistry::from_config(tmp.path(), &[]);
        let tools = reg.merged_tools();
        let mut names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        let mut expected: Vec<&str> = BUILTIN_TOOLS.iter().map(|t| t.name).collect();
        names.sort_unstable();
        expected.sort_unstable();
        assert_eq!(names, expected);
    }
}

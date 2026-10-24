//! Bundled tools and their registry.
//!
//! Tools are executable capabilities the model can invoke (`file_read`,
//! `file_write`, `bash`). They are deliberately kept separate from
//! [`crate::capabilities::skills::Skill`]: a skill contributes guidance to the
//! system prompt, while a tool is something the agent can actually call.
//! [`ToolRegistry`] owns the mounted set and resolves the user's `tools:`
//! config list.
//!
//! The tools speak the [`oven_agent::Tool`] protocol; this layer owns the
//! capabilities themselves, not the protocol they speak.

mod answer;
mod bash;
mod catalog;
mod file_edit;
mod file_read;
mod file_write;
mod glob;
mod grep;
mod list_models;
mod matching;
mod skill_read;
mod task;
mod todo_write;
mod web_fetch;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::string::String;
use std::sync::Arc;

use oven_agent::{AgentError, Tool};
use oven_proto::ToolView;
use serde_json::Value;

#[cfg(test)]
use oven_agent::{AgentMode, CancellationToken, Selection, TurnContext};
#[cfg(test)]
use oven_llm::ModelId;
#[cfg(test)]
use oven_proto::TurnId;

pub use answer::AnswerTool;
pub use bash::BashTool;
pub use catalog::{BUILTIN_TOOLS, BuiltinTool};
pub use file_edit::FileEditTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use list_models::ListModelsTool;
pub use skill_read::SkillReadTool;
pub use task::{TaskOutputTool, TaskTool};
pub use todo_write::TodoWriteTool;
pub use web_fetch::WebFetchTool;

/// A factory producing one tool instance. Tools are rebuilt on every agent
/// spawn, so the registry hands out fresh instances on demand.
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
        let requested: BTreeSet<&str> = requested.iter().map(String::as_str).collect();
        let selected: Vec<&BuiltinTool> = if requested.is_empty() {
            BUILTIN_TOOLS.iter().collect()
        } else {
            BUILTIN_TOOLS
                .iter()
                .filter(|tool| requested.contains(tool.name))
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
        self.tools.keys().map(String::as_str).collect()
    }

    /// Fresh tool instances for one agent, in registry order. They come back
    /// as shared handles so the main agent and the subagents it spawns mount
    /// the same instances instead of reconnecting MCP servers per child.
    pub fn merged_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools
            .values()
            .map(|make| Arc::from(make()) as Arc<dyn Tool>)
            .collect()
    }
}

fn resolve_within(root: &Path, rel: &str) -> Result<PathBuf, AgentError> {
    oven_host::resolve_within(root, rel).map_err(|error| AgentError::from(error.to_string()))
}

fn require_str<'a>(args: &'a Value, key: &str, tool: &str) -> Result<&'a str, AgentError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| AgentError::from(format!("{tool}: missing '{key}' string argument")))
}

fn parse_limit(args: &Value, default: usize) -> usize {
    args.get("limit")
        .and_then(Value::as_i64)
        .map(|v| v.max(0))
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(default)
}

/// Summary for a call keyed on one argument, e.g. `Edited src/main.rs`.
///
/// Falls back to the bare verb, not the tool name, so a model that emits
/// malformed arguments still renders as an action — `Edited 1 file` — instead
/// of a raw `file_edit` line the frontend cannot group.
fn labeled(verb: &str, input: &Value, key: &str) -> ToolView {
    match input_str(input, key) {
        Some(v) => ToolView {
            summary: format!("{verb} {v}"),
            collapse: true,
            detail: None,
        },
        None => ToolView::named(verb),
    }
}

fn input_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// A bare turn for tests that exercise a tool without driving a turn.
#[cfg(test)]
fn test_context() -> TurnContext {
    test_context_in(AgentMode::Agent)
}

/// A bare turn in the given mode, for tests that exercise what a mode allows.
#[cfg(test)]
fn test_context_in(mode: AgentMode) -> TurnContext {
    TurnContext::new(
        TurnId::next(),
        CancellationToken::new(),
        Selection::new(mode, ModelId::new("default"), None),
    )
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

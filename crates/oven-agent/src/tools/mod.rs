mod bash;
mod file_edit;
mod file_read;
mod file_write;
mod glob;
mod grep;
mod skill_read;
mod todo_write;

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;

pub use bash::BashTool;
pub use file_edit::FileEditTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use skill_read::SkillReadTool;
pub use todo_write::TodoWriteTool;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> Value;
    async fn run(
        &self,
        args: &Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError>;
}

pub(crate) fn resolve_within(root: &Path, rel: &str) -> Result<PathBuf, AgentError> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Err(AgentError::from("missing path argument"));
    }
    for comp in Path::new(rel).components() {
        if matches!(comp, Component::ParentDir) {
            return Err(AgentError::from(format!("path escapes root: {}", rel)));
        }
    }
    Ok(root.join(rel))
}

pub(crate) fn require_str<'a>(
    args: &'a Value,
    key: &str,
    tool: &str,
) -> Result<&'a str, AgentError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| AgentError::from(format!("{}: missing '{}' string argument", tool, key)))
}

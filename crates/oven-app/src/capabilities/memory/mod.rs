mod forget;
mod read;
mod view;
mod write;

use oven_agent::AgentError;
use oven_mem::{MemoryError, MemoryId, MemoryKind, MemoryScope};

pub(crate) use forget::MemoryForgetTool;
pub(crate) use read::MemoryReadTool;
pub(crate) use view::present_memory_tool;
pub(crate) use write::MemoryWriteTool;

const UNKNOWN_SCOPE: &str = "unknown memory scope";

pub(crate) fn parse_scope(tool: &str, raw: &str) -> Result<MemoryScope, AgentError> {
    if raw == MemoryScope::Workspace.as_str() {
        return Ok(MemoryScope::Workspace);
    }
    if raw == MemoryScope::User.as_str() {
        return Ok(MemoryScope::User);
    }
    Err(AgentError::from(format!("{tool}: {UNKNOWN_SCOPE} '{raw}'")))
}

pub(crate) fn parse_id(raw: &str) -> Result<MemoryId, AgentError> {
    MemoryId::new(raw).map_err(|err| AgentError::from(err.to_string()))
}

pub(crate) fn parse_kind(raw: &str) -> Result<MemoryKind, AgentError> {
    if raw == MemoryKind::Fact.as_str() {
        return Ok(MemoryKind::Fact);
    }
    if raw == MemoryKind::Preference.as_str() {
        return Ok(MemoryKind::Preference);
    }
    Err(AgentError::from(
        MemoryError::UnknownKind {
            kind: raw.to_owned(),
        }
        .to_string(),
    ))
}

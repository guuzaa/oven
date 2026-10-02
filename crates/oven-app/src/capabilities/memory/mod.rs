mod read;

use oven_agent::AgentError;
use oven_mem::{MemoryId, MemoryScope};

pub(crate) use read::MemoryReadTool;

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

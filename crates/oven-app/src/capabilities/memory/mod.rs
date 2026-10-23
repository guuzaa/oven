mod forget;
mod read;
mod view;
mod write;

use oven_agent::AgentError;
use oven_mem::{MemoryError, MemoryId, MemoryKind, MemoryScope};
use serde_json::{Value, json};

pub(crate) use forget::MemoryForgetTool;
pub(crate) use read::MemoryReadTool;
pub(crate) use view::present_memory_tool;
pub(crate) use write::MemoryWriteTool;

/// The scope a memory tool's argument names, named after the tool that asked
/// for it.
pub(crate) fn parse_scope(tool: &str, raw: &str) -> Result<MemoryScope, AgentError> {
    crate::core::memory::parse_scope(raw)
        .map_err(|message| AgentError::from(format!("{tool}: {message}")))
}

/// The id a memory tool's argument names.
pub(crate) fn parse_id(raw: &str) -> Result<MemoryId, AgentError> {
    crate::core::memory::parse_id(raw).map_err(AgentError::from)
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

/// A required string argument, or the message naming the tool that wanted
/// it.
pub(crate) fn required_str<'a>(
    args: &'a Value,
    key: &str,
    tool: &str,
) -> Result<&'a str, AgentError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::from(format!("{tool}: missing '{key}' string argument")))
}

/// The `scope` property every memory tool's schema carries.
pub(crate) fn scope_property() -> Value {
    json!({
        "type": "string",
        "enum": ["workspace", "user"],
        "description": "workspace for this repository, user for personal preferences"
    })
}

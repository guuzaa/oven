mod forget;
mod note;
mod read;
mod remember;
mod view;

use oven_mem::{MemoryId, MemoryKind, MemoryScope};
use oven_proto::AgentError;
use serde_json::{Value, json};

pub(crate) use forget::MemoryForgetTool;
pub(crate) use note::MemoryNoteTool;
pub(crate) use read::MemoryReadTool;
pub(crate) use remember::MemoryRememberTool;
pub(crate) use view::present_memory_tool;

const REF_KEY: &str = "ref";
const KIND_KEY: &str = "kind";
const UNKNOWN_KIND: &str = "unknown memory kind";
const EXPECTED_KINDS: &str = "expected fact or preference";
const MISSING_SLASH: &str = "memory ref must be <scope>/<id>";

/// The `scope/id` a memory tool's `ref` argument names.
pub(crate) fn parse_ref(args: &Value, tool: &str) -> Result<(MemoryScope, MemoryId), AgentError> {
    parse_ref_str(required_str(args, REF_KEY, tool)?, tool)
}

pub(crate) fn parse_ref_str(raw: &str, tool: &str) -> Result<(MemoryScope, MemoryId), AgentError> {
    let Some((scope, id)) = raw.split_once('/') else {
        return Err(AgentError::from(format!(
            "{tool}: {MISSING_SLASH}: '{raw}'"
        )));
    };
    let scope = crate::core::memory::parse_scope(scope)
        .map_err(|message| AgentError::from(format!("{tool}: {message}")))?;
    let id = crate::core::memory::parse_id(id).map_err(AgentError::from)?;
    Ok((scope, id))
}

pub(crate) fn parse_kind(args: &Value, tool: &str) -> Result<MemoryKind, AgentError> {
    let raw = required_str(args, KIND_KEY, tool)?;
    MemoryKind::parse(raw).ok_or_else(|| {
        AgentError::from(format!("{tool}: {UNKNOWN_KIND} '{raw}'; {EXPECTED_KINDS}"))
    })
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

/// An optional string argument; blank counts as absent.
pub(crate) fn optional_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// The `ref` property of the tools that name one memory.
pub(crate) fn ref_property() -> Value {
    json!({
        "type": "string",
        "description": "The memory as the catalog names it: project/<id>, shared/<id> or global/<id>."
    })
}

pub(crate) fn kind_property() -> Value {
    json!({
        "type": "string",
        "enum": ["fact", "preference"],
        "description": "fact: about the code or environment. preference: about how the user wants you to work."
    })
}

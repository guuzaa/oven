//! The vocabulary every memory path shares: how a reference is parsed and
//! how a memory is shown.

use std::fmt::Write;

use oven_mem::{Memory, MemoryId, MemoryScope};

pub(crate) const UNKNOWN_SCOPE: &str = "unknown memory scope";
pub(crate) const KIND_LABEL: &str = "kind";
pub(crate) const DESCRIPTION_LABEL: &str = "description";
pub(crate) const SOURCE_LABEL: &str = "source";

/// The scope a memory reference names, or the message to show when it names
/// none.
pub(crate) fn parse_scope(raw: &str) -> Result<MemoryScope, String> {
    if raw == MemoryScope::Workspace.as_str() {
        return Ok(MemoryScope::Workspace);
    }
    if raw == MemoryScope::User.as_str() {
        return Ok(MemoryScope::User);
    }
    Err(format!("{UNKNOWN_SCOPE} '{raw}'"))
}

/// The id a memory reference names, or the message to show when it names
/// none.
pub(crate) fn parse_id(raw: &str) -> Result<MemoryId, String> {
    MemoryId::new(raw).map_err(|err| err.to_string())
}

/// The header every rendering of a memory opens with: its kind and its
/// description.
pub(crate) fn header(memory: &Memory) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{KIND_LABEL}: {}", memory.kind.as_str());
    let _ = writeln!(out, "{DESCRIPTION_LABEL}: {}", memory.description);
    out
}

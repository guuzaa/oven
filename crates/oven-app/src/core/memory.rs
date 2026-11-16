//! The vocabulary every memory path shares: how a reference is parsed and
//! how a memory is shown.

use std::fmt::Write;

use oven_mem::{Memory, MemoryId, MemoryScope, MemoryStatus};

pub(crate) const UNKNOWN_SCOPE: &str = "unknown memory scope";
pub(crate) const EXPECTED_SCOPES: &str = "expected project, shared or global";
pub(crate) const TYPE_LABEL: &str = "type";
pub(crate) const DESCRIPTION_LABEL: &str = "description";
pub(crate) const WHY_LABEL: &str = "why";
pub(crate) const STATUS_LABEL: &str = "status";
pub(crate) const REVIEWED_LABEL: &str = "reviewed by";
pub(crate) const ANCHORS_LABEL: &str = "anchors";
pub(crate) const STALE_AFTER_LABEL: &str = "stale after";
pub(crate) const SOURCES_LABEL: &str = "sources";

const LIST_SEPARATOR: &str = ", ";

/// The scope a memory reference names, or the message to show when it names
/// none.
pub(crate) fn parse_scope(raw: &str) -> Result<MemoryScope, String> {
    MemoryScope::parse(raw).ok_or_else(|| format!("{UNKNOWN_SCOPE} '{raw}'; {EXPECTED_SCOPES}"))
}

/// The id a memory reference names, or the message to show when it names
/// none.
pub(crate) fn parse_id(raw: &str) -> Result<MemoryId, String> {
    MemoryId::new(raw).map_err(|err| err.to_string())
}

/// A memory the way both `memory_read` and `/memory show` print it: the
/// frontmatter that matters as labelled lines, then the body.
pub(crate) fn describe(memory: &Memory) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{TYPE_LABEL}: {}", memory.kind.as_str());
    let _ = writeln!(out, "{DESCRIPTION_LABEL}: {}", memory.description);
    if let Some(why) = &memory.why {
        let _ = writeln!(out, "{WHY_LABEL}: {why}");
    }
    if memory.status != MemoryStatus::Stable {
        let _ = writeln!(out, "{STATUS_LABEL}: {}", memory.status.as_str());
    }
    let reviewers: Vec<&str> = memory
        .verified
        .iter()
        .filter(|stamp| stamp.is_human())
        .map(|stamp| stamp.by.as_str())
        .collect();
    if !reviewers.is_empty() {
        let _ = writeln!(out, "{REVIEWED_LABEL}: {}", reviewers.join(LIST_SEPARATOR));
    }
    if !memory.anchors.is_empty() {
        let _ = writeln!(
            out,
            "{ANCHORS_LABEL}: {}",
            memory.anchors.join(LIST_SEPARATOR)
        );
    }
    if let Some(stale_after) = &memory.stale_after {
        let _ = writeln!(out, "{STALE_AFTER_LABEL}: {stale_after}");
    }
    if !memory.sources.is_empty() {
        let resources: Vec<&str> = memory.sources.iter().map(|s| s.resource.as_str()).collect();
        let _ = writeln!(out, "{SOURCES_LABEL}: {}", resources.join(LIST_SEPARATOR));
    }
    if !memory.body.is_empty() {
        out.push('\n');
        out.push_str(&memory.body);
    }
    out
}

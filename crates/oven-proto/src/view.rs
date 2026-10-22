//! What a tool call looks like to a frontend.

use serde::{Deserialize, Serialize};

/// What a tool call looks like to a frontend, and what it may do.
///
/// The nouns the rest of the protocol reasons about — an event carries a
/// [`ToolView`], a turn's gate reads [`ToolPermission`](crate::ToolPermission)
/// — so they sit here rather than beside the tools that produce them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolView {
    pub summary: String,
    pub collapse: bool,
    /// Nested body rendered under the summary, e.g. a file diff.
    pub detail: Option<String>,
}

impl ToolView {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            summary: name.into(),
            collapse: true,
            detail: None,
        }
    }
}

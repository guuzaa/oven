//! What a tool call looks like to a frontend, and what it may do.
//!
//! These are the nouns the rest of the crate reasons about — an event carries
//! a [`ToolView`], a turn's gate reads [`ToolPermission`], a tool declares
//! [`ToolCaps`] — so they sit below the tools that produce them. The built-in
//! presentation table lives in [`crate::capabilities::tools`].

#[derive(Debug, Clone, PartialEq, Eq)]
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

/// What a tool may do to the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolPermission {
    #[default]
    Read,
    Write,
    Execute,
    External,
}

/// What a tool asks of the turn it runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ToolCaps {
    pub plan_only: bool,
    pub permission: ToolPermission,
    /// Runs on its own, never alongside another call of the same step.
    ///
    /// A step's calls run at the same time, so a tool that reads a file,
    /// changes it and writes it back would lose one of two edits to the same
    /// file; so would a tool that needs the frontend's attention to itself.
    pub exclusive: bool,
}

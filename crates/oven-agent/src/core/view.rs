//! What a tool hands back, and what it asks of the turn it runs in.

use oven_proto::TodoList;
use oven_proto::ToolPermission;

/// What a tool hands back: the text the history keeps, and the body a
/// frontend shows under the call once it has landed.
///
/// A tool learns things while it runs — where an edit matched, which option
/// the user picked — so it reports the landed body itself rather than leaving
/// the frontend to re-derive it from the arguments it was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub output: String,
    pub detail: Option<String>,
}

impl From<&str> for ToolOutcome {
    fn from(output: &str) -> Self {
        Self {
            output: output.to_string(),
            detail: None,
        }
    }
}

impl From<String> for ToolOutcome {
    fn from(output: String) -> Self {
        Self {
            output,
            detail: None,
        }
    }
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

/// What a tool call leaves for the driver to record besides its output.
///
/// A tool that changes state the driver carries says so here, so the driver
/// records it without knowing which tool was called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolEffect {
    /// The checklist the model rewrote.
    Todos(TodoList),
}

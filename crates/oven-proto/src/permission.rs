//! What a tool may do to the workspace.

use serde::{Deserialize, Serialize};

/// What a tool may do to the workspace. The mode decides what each
/// permission means: an ask-mode session may read and ask before it writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermission {
    #[default]
    Read,
    Write,
    Execute,
    External,
}

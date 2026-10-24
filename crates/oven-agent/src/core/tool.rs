//! The protocol one callable capability speaks.
//!
//! The protocol lives here, the capabilities live in the app layer: this
//! crate drives turns and never names a tool it was not handed.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::core::turn::TurnContext;
use crate::core::view::{ToolCaps, ToolEffect, ToolOutcome};
use oven_proto::AgentError;
use oven_proto::ToolView;

/// One capability the model can call. `run` receives the whole
/// [`TurnContext`] rather than a bag of options, so a tool that starts work
/// of its own — a subagent, a nested loop — inherits the run it was called
/// from instead of guessing.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// The tool's JSON Schema. Shared, so a caller that keeps it does not
    /// copy the schema tree, and a tool that never changes it can hand out
    /// the same one every time.
    fn schema(&self) -> Arc<Value>;
    /// How this call reads in a frontend. A tool that knows better than its
    /// own name overrides it.
    fn view(&self, _input: &Value) -> ToolView {
        ToolView::named(self.name())
    }
    fn caps(&self) -> ToolCaps {
        ToolCaps::default()
    }
    /// What this call leaves behind for the driver to record, e.g. a
    /// checklist the model rewrote. `None` for a call that leaves nothing.
    fn effect(&self, _input: &Value) -> Option<ToolEffect> {
        None
    }
    async fn run(&self, args: &Value, cx: &TurnContext) -> Result<ToolOutcome, AgentError>;
}

//! How a tool call renders when only its name and input survive, e.g. when
//! replaying a saved session: the agent's built-in table plus the app's tools.

use oven_agent::ToolView;
use serde_json::Value;

use super::memory::present_memory_tool;

pub fn present_tool(name: &str, input: &Value) -> ToolView {
    present_memory_tool(name, input).unwrap_or_else(|| oven_agent::present_tool(name, input))
}

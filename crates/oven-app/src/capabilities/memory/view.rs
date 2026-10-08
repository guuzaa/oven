use oven_agent::ToolView;
use serde_json::Value;

use super::{MemoryForgetTool, MemoryReadTool, MemoryWriteTool};

pub(crate) fn present_memory_tool(name: &str, input: &Value) -> Option<ToolView> {
    match name {
        MemoryWriteTool::NAME => Some(MemoryWriteTool::view_input(input)),
        MemoryReadTool::NAME => Some(MemoryReadTool::view_input(input)),
        MemoryForgetTool::NAME => Some(MemoryForgetTool::view_input(input)),
        _ => None,
    }
}

pub(super) fn memory_view(verb: &str, input: &Value) -> ToolView {
    let field = |key| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    match (field("scope"), field("id")) {
        (Some(scope), Some(id)) => ToolView {
            summary: format!("{verb} {scope}/{id}"),
            collapse: true,
            detail: None,
        },
        _ => ToolView::named(verb),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{MemoryForgetTool, MemoryReadTool, MemoryWriteTool, present_memory_tool};

    const INPUT_ID: &str = "proxy-requires-http2";

    fn summary(name: &str) -> String {
        present_memory_tool(name, &json!({ "scope": "workspace", "id": INPUT_ID }))
            .unwrap()
            .summary
    }

    #[test]
    fn memory_calls_read_as_human_actions() {
        assert_eq!(
            summary(MemoryWriteTool::NAME),
            format!("{} workspace/{INPUT_ID}", MemoryWriteTool::VERB)
        );
        assert_eq!(
            summary(MemoryReadTool::NAME),
            format!("{} workspace/{INPUT_ID}", MemoryReadTool::VERB)
        );
        assert_eq!(
            summary(MemoryForgetTool::NAME),
            format!("{} workspace/{INPUT_ID}", MemoryForgetTool::VERB)
        );
    }

    #[test]
    fn incomplete_input_falls_back_to_the_verb() {
        let view = present_memory_tool(MemoryWriteTool::NAME, &json!({ "id": INPUT_ID })).unwrap();
        assert_eq!(view.summary, MemoryWriteTool::VERB);
    }

    #[test]
    fn other_tools_are_not_memory_tools() {
        assert_eq!(present_memory_tool("bash", &json!({})), None);
    }
}

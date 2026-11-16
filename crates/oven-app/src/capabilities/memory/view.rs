use oven_proto::ToolView;
use serde_json::Value;

use super::{MemoryForgetTool, MemoryNoteTool, MemoryReadTool, MemoryRememberTool};

pub(crate) fn present_memory_tool(name: &str, input: &Value) -> Option<ToolView> {
    match name {
        MemoryNoteTool::NAME => Some(note_view(input)),
        MemoryRememberTool::NAME => Some(memory_view(MemoryRememberTool::VERB, input)),
        MemoryReadTool::NAME => Some(memory_view(MemoryReadTool::VERB, input)),
        MemoryForgetTool::NAME => Some(memory_view(MemoryForgetTool::VERB, input)),
        _ => None,
    }
}

fn field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// `Recalled project/proxy`: the verb and the memory, from a `ref` or from
/// a `scope` and an `id`.
pub(super) fn memory_view(verb: &str, input: &Value) -> ToolView {
    let reference = field(input, "ref").map(str::to_owned).or_else(|| {
        let (scope, id) = (field(input, "scope")?, field(input, "id")?);
        Some(format!("{scope}/{id}"))
    });
    match reference {
        Some(reference) => collapsed(format!("{verb} {reference}")),
        None => ToolView::named(verb),
    }
}

/// `Noted preference`: a note has no id yet, so it is named by its kind.
pub(super) fn note_view(input: &Value) -> ToolView {
    match field(input, "kind") {
        Some(kind) => collapsed(format!("{} {kind}", MemoryNoteTool::VERB)),
        None => ToolView::named(MemoryNoteTool::VERB),
    }
}

fn collapsed(summary: String) -> ToolView {
    ToolView {
        summary,
        collapse: true,
        detail: None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        MemoryForgetTool, MemoryNoteTool, MemoryReadTool, MemoryRememberTool, present_memory_tool,
    };

    const REF: &str = "project/proxy";

    fn summary(name: &str, input: serde_json::Value) -> String {
        present_memory_tool(name, &input).unwrap().summary
    }

    #[test]
    fn memory_calls_read_as_human_actions() {
        let by_ref = json!({ "ref": REF });
        assert_eq!(
            summary(MemoryReadTool::NAME, by_ref.clone()),
            format!("{} {REF}", MemoryReadTool::VERB)
        );
        assert_eq!(
            summary(MemoryForgetTool::NAME, by_ref),
            format!("{} {REF}", MemoryForgetTool::VERB)
        );
        assert_eq!(
            summary(
                MemoryRememberTool::NAME,
                json!({ "scope": "project", "id": "proxy" })
            ),
            format!("{} {REF}", MemoryRememberTool::VERB)
        );
        assert_eq!(
            summary(MemoryNoteTool::NAME, json!({ "kind": "fact" })),
            format!("{} fact", MemoryNoteTool::VERB)
        );
    }

    #[test]
    fn incomplete_input_falls_back_to_the_verb() {
        assert_eq!(
            summary(MemoryRememberTool::NAME, json!({ "id": "proxy" })),
            MemoryRememberTool::VERB
        );
        assert_eq!(
            summary(MemoryNoteTool::NAME, json!({})),
            MemoryNoteTool::VERB
        );
    }

    #[test]
    fn other_tools_are_not_memory_tools() {
        assert_eq!(present_memory_tool("bash", &json!({})), None);
    }
}

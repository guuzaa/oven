//! `Request`: one conversation, provider independent.

use serde::{Deserialize, Serialize};

use super::message::Message;
use super::tool::{Tool, ToolChoice};

/// The content of one LLM call, provider independent.
///
/// The model is not here: it is the target of a call and lives in
/// [`CallOptions`](super::options::CallOptions), next to the sampling
/// parameters and the reasoning effort. Another model does not mean another
/// conversation.
///
/// `system` is the system prompt of the conversation: the protocols carry it
/// apart from the messages (the Responses API's `instructions`, the first
/// `system` message of a Chat Completions request). A `Role::System` *message*
/// is a different thing — one message of the conversation that happens to be a
/// system message.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Request {
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<Tool>,
    pub tool_choice: ToolChoice,
}

impl Request {
    /// 设置 system 提示。
    pub fn system(&mut self, system: impl Into<String>) -> &mut Self {
        self.system = Some(system.into());
        self
    }

    /// 追加一条消息。
    pub fn push_message(&mut self, msg: Message) -> &mut Self {
        self.messages.push(msg);
        self
    }

    /// 替换全部消息。
    pub fn set_messages(&mut self, msgs: Vec<Message>) -> &mut Self {
        self.messages = msgs;
        self
    }

    /// 追加一个工具。
    pub fn push_tool(&mut self, tool: Tool) -> &mut Self {
        self.tools.push(tool);
        self
    }

    /// 替换全部工具。
    pub fn set_tools(&mut self, tools: Vec<Tool>) -> &mut Self {
        self.tools = tools;
        self
    }

    /// 设置 tool_choice。
    pub fn tool_choice(&mut self, choice: ToolChoice) -> &mut Self {
        self.tool_choice = choice;
        self
    }
}

// ---------------------------------------------------------------------------
// Builder（消费式 `self` 方法，用于构造阶段的链式调用）
// ---------------------------------------------------------------------------

/// Builds a [`Request`]. See [`Request::builder()`].
///
/// The methods say what they do: `push_*` adds one, `set_*` replaces the
/// whole collection.
///
/// ```rust
/// use oven_llm::*;
///
/// let req = Request::builder()
///     .system("be helpful")
///     .push_message(Message::user(vec![ContentBlock::text("hello")]))
///     .build();
/// ```
#[derive(Debug, Default)]
pub struct RequestBuilder {
    system: Option<String>,
    messages: Vec<Message>,
    tools: Vec<Tool>,
    tool_choice: ToolChoice,
}

impl RequestBuilder {
    /// 设置 system 提示。
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// 追加一条消息。
    pub fn push_message(mut self, msg: Message) -> Self {
        self.messages.push(msg);
        self
    }

    /// 追加一条用户文本消息。
    pub fn prompt(mut self, msg: impl Into<String>) -> Self {
        self.messages.push(Message::user_text(msg.into()));
        self
    }

    /// 追加一个工具。
    pub fn push_tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    /// 替换全部工具。
    pub fn set_tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = tools;
        self
    }

    /// 设置 tool_choice。
    pub fn tool_choice(mut self, choice: ToolChoice) -> Self {
        self.tool_choice = choice;
        self
    }

    /// 替换全部消息。
    pub fn set_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    /// Builds the [`Request`].
    pub fn build(self) -> Request {
        Request {
            system: self.system,
            messages: self.messages,
            tools: self.tools,
            tool_choice: self.tool_choice,
        }
    }
}

impl Request {
    /// 创建 [`RequestBuilder`]。
    pub fn builder() -> RequestBuilder {
        RequestBuilder::default()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::domain::message::{ContentBlock, Role};

    #[test]
    fn request_default_has_empty_collections() {
        let req = Request::default();
        assert_eq!(req.system, None);
        assert!(req.messages.is_empty());
        assert!(req.tools.is_empty());
        assert_eq!(req.tool_choice, ToolChoice::Auto);
    }

    #[test]
    fn request_round_trips() {
        let req = Request {
            system: Some("be helpful".to_string()),
            messages: vec![Message::user(vec![ContentBlock::text("hi")])],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.system, req.system);
        assert_eq!(decoded.tool_choice, req.tool_choice);
        assert_eq!(decoded.messages[0].role, Role::User);
    }

    #[test]
    fn builder_chain_produces_correct_request() {
        let req = Request::builder()
            .system("be helpful")
            .push_message(Message::user(vec![ContentBlock::text("hello")]))
            .push_message(Message::assistant(vec![ContentBlock::text("hi")]))
            .push_tool(Tool {
                name: "search".into(),
                description: Some("Search the web".into()),
                input_schema: Arc::new(serde_json::json!({"type": "object"})),
            })
            .tool_choice(ToolChoice::Auto)
            .build();

        assert_eq!(req.system.as_deref(), Some("be helpful"));
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[1].role, Role::Assistant);
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "search");
        assert_eq!(req.tool_choice, ToolChoice::Auto);
    }

    #[test]
    fn fluent_setters_chain_correctly() {
        let mut req = Request::default();
        req.system("be helpful").tool_choice(ToolChoice::Any);

        assert_eq!(req.system.as_deref(), Some("be helpful"));
        assert_eq!(req.tool_choice, ToolChoice::Any);
    }

    #[test]
    fn push_message_appends() {
        let mut req = Request::default();
        req.push_message(Message::user(vec![ContentBlock::text("hello")]))
            .push_message(Message::assistant(vec![ContentBlock::text("hi")]));

        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[1].role, Role::Assistant);
    }

    #[test]
    fn set_messages_replaces() {
        let mut req = Request::default();
        req.push_message(Message::user(vec![ContentBlock::text("old")]))
            .set_messages(vec![Message::user(vec![ContentBlock::text("new")])]);

        assert_eq!(req.messages.len(), 1);
        // Verify the new content is present (check via serialization roundtrip).
        let json = serde_json::to_value(&req.messages[0]).unwrap();
        assert_eq!(json["content"][0]["text"], "new");
    }

    #[test]
    fn push_tool_appends() {
        let mut req = Request::default();
        req.push_tool(Tool {
            name: "a".into(),
            description: None,
            input_schema: Arc::new(serde_json::json!({})),
        })
        .push_tool(Tool {
            name: "b".into(),
            description: None,
            input_schema: Arc::new(serde_json::json!({})),
        });

        assert_eq!(req.tools.len(), 2);
        assert_eq!(req.tools[0].name, "a");
        assert_eq!(req.tools[1].name, "b");
    }

    #[test]
    fn set_tools_replaces() {
        let mut req = Request::default();
        req.push_tool(Tool {
            name: "old".into(),
            description: None,
            input_schema: Arc::new(serde_json::json!({})),
        })
        .set_tools(vec![Tool {
            name: "new".into(),
            description: None,
            input_schema: Arc::new(serde_json::json!({})),
        }]);

        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "new");
    }

    #[test]
    fn realistic_construction_and_mutation() {
        // Construction phase — use builder
        let mut req = Request::builder()
            .system("You are a coding assistant.")
            .push_message(Message::user(vec![ContentBlock::text(
                "Write hello world in Rust.",
            )]))
            .push_tool(Tool {
                name: "run_code".into(),
                description: Some("Execute code".into()),
                input_schema: Arc::new(serde_json::json!({"type": "object"})),
            })
            .build();

        // Simulate agent loop mutation
        req.push_message(Message::assistant(vec![
            ContentBlock::thinking("Let me write code."),
            ContentBlock::text(
                "Here is the code:\n```rust\nfn main() { println!(\"Hello!\"); }\n```",
            ),
        ]))
        .push_message(Message::user(vec![ContentBlock::text("Run it.")]));

        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[1].role, Role::Assistant);
        assert_eq!(req.messages[2].role, Role::User);
        assert_eq!(req.tools.len(), 1);
    }
}

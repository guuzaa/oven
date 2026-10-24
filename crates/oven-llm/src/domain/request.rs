//! `ModelId` / `Request`：provider 无关的请求模型。

use std::{borrow::Borrow, fmt};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::message::Message;
use super::tool::{Tool, ToolChoice};

/// 模型标识：完整 slug（`vendor/wire-id[:variant]`）或裸 wire id。
///
/// serde 仍是普通字符串。发上游时只用 [`wire_id`](Self::wire_id)，不要把
/// `vendor/` 前缀写进请求体。
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 构造时写入的完整字符串（可能是 slug，也可能是裸 id）。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// slug 里的 vendor 段（未规范化）。裸 id 返回 `None`。
    pub fn vendor(&self) -> Option<&str> {
        let (vendor, _, _) = split_model_id(&self.0);
        vendor
    }

    /// 发给上游的模型名：去掉 vendor 前缀和 `:variant`。
    pub fn wire_id(&self) -> &str {
        let (_, wire, _) = split_model_id(&self.0);
        wire
    }

    /// 协议变体，例如 `responses`。picker 不展示；手打 slug 时使用。
    pub fn variant(&self) -> Option<&str> {
        let (_, _, variant) = split_model_id(&self.0);
        variant
    }

    /// 把裸 id 补成 `vendor/wire-id[:variant]`。已有 vendor 时只规范 vendor 段。
    pub fn qualify(&self, vendor: &str) -> Self {
        let vendor = canonical_vendor(vendor);
        match self.variant() {
            Some(variant) => Self(format!("{vendor}/{}:{variant}", self.wire_id())),
            None => Self(format!("{vendor}/{}", self.wire_id())),
        }
    }
}

/// 已知厂商别名 → 规范 slug（小写）。未知值转成小写后原样返回。
pub fn canonical_vendor(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "kimi" | "moonshot" => "moonshot".to_string(),
        "glm" | "zhipu" => "zhipu".to_string(),
        "grok" | "xai" => "xai".to_string(),
        other => other.to_string(),
    }
}

fn split_model_id(raw: &str) -> (Option<&str>, &str, Option<&str>) {
    let (main, variant) = match raw.rsplit_once(':') {
        Some((left, right)) if !left.is_empty() && !right.is_empty() && !right.contains('/') => {
            (left, Some(right))
        }
        _ => (raw, None),
    };
    match main.split_once('/') {
        Some((vendor, wire)) if !vendor.is_empty() && !wire.is_empty() => {
            (Some(vendor), wire, variant)
        }
        _ => (None, main, variant),
    }
}

impl From<String> for ModelId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ModelId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl AsRef<str> for ModelId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for ModelId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 一次 LLM 调用的完整请求，与具体 provider 无关。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Request {
    pub model: ModelId,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<Tool>,
    pub tool_choice: ToolChoice,
}

impl Request {
    /// 设置模型标识。
    pub fn model(&mut self, model: impl Into<ModelId>) -> &mut Self {
        self.model = model.into();
        self
    }

    /// 设置 system 提示。
    pub fn system(&mut self, system: impl Into<String>) -> &mut Self {
        self.system = Some(system.into());
        self
    }

    /// 追加一条消息。
    pub fn message(&mut self, msg: Message) -> &mut Self {
        self.messages.push(msg);
        self
    }

    /// 替换全部消息。
    pub fn messages(&mut self, msgs: Vec<Message>) -> &mut Self {
        self.messages = msgs;
        self
    }

    /// 追加一个工具。
    pub fn tool(&mut self, tool: Tool) -> &mut Self {
        self.tools.push(tool);
        self
    }

    /// 替换全部工具。
    pub fn tools(&mut self, tools: Vec<Tool>) -> &mut Self {
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

/// [`RequestBuilder`] 构建失败的错误类型。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuilderError {
    /// 必需字段 `model` 未设置。
    #[error("builder missing required field: model")]
    MissingModel,
}

/// [`Request`] 的构造器。通过 [`Request::builder()`] 创建。
///
/// ```rust
/// use oven_llm::*;
///
/// let req = Request::builder()
///     .model("gpt-4")
///     .system("be helpful")
///     .message(Message::user(vec![ContentBlock::text("hello")]))
///     .build()
///     .unwrap();
/// ```
#[derive(Debug, Default)]
pub struct RequestBuilder {
    model: Option<ModelId>,
    system: Option<String>,
    messages: Vec<Message>,
    tools: Vec<Tool>,
    tool_choice: ToolChoice,
}

impl RequestBuilder {
    /// 设置模型标识。
    pub fn model(mut self, model: impl Into<ModelId>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// 设置 system 提示。
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// 追加一条消息。
    pub fn message(mut self, msg: Message) -> Self {
        self.messages.push(msg);
        self
    }

    pub fn prompt(mut self, msg: impl Into<String>) -> Self {
        self.messages.push(Message::user_text(msg.into()));
        self
    }

    /// 追加一个工具。
    pub fn tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    /// 替换全部工具。
    pub fn tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = tools;
        self
    }

    /// 设置 tool_choice。
    pub fn tool_choice(mut self, choice: ToolChoice) -> Self {
        self.tool_choice = choice;
        self
    }

    /// 替换全部消息。
    pub fn messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    /// 构建 [`Request`]。未调用 [`model`](Self::model) 时返回错误。
    pub fn build(self) -> Result<Request, BuilderError> {
        let model = self.model.ok_or(BuilderError::MissingModel)?;
        Ok(Request {
            model,
            system: self.system,
            messages: self.messages,
            tools: self.tools,
            tool_choice: self.tool_choice,
        })
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
    fn model_id_parses_slug_variant_and_bare() {
        let slug = ModelId::from("deepseek/deepseek-v4-flash:responses");
        assert_eq!(slug.vendor(), Some("deepseek"));
        assert_eq!(slug.wire_id(), "deepseek-v4-flash");
        assert_eq!(slug.variant(), Some("responses"));
        assert_eq!(slug.as_str(), "deepseek/deepseek-v4-flash:responses");

        let nested = ModelId::from("my-proxy/org/model");
        assert_eq!(nested.vendor(), Some("my-proxy"));
        assert_eq!(nested.wire_id(), "org/model");
        assert_eq!(nested.variant(), None);

        let bare = ModelId::from("deepseek-v4-flash");
        assert_eq!(bare.vendor(), None);
        assert_eq!(bare.wire_id(), "deepseek-v4-flash");
        assert_eq!(bare.variant(), None);

        let bare_variant = ModelId::from("deepseek-v4-flash:responses");
        assert_eq!(bare_variant.vendor(), None);
        assert_eq!(bare_variant.wire_id(), "deepseek-v4-flash");
        assert_eq!(bare_variant.variant(), Some("responses"));
    }

    #[test]
    fn model_id_qualify_and_canonical_vendor() {
        let qualified = ModelId::from("deepseek-v4-flash:responses").qualify("kimi");
        assert_eq!(qualified.as_str(), "moonshot/deepseek-v4-flash:responses");
        assert_eq!(canonical_vendor("GROK"), "xai");
        assert_eq!(canonical_vendor("glm"), "zhipu");
        assert_eq!(canonical_vendor("My-Proxy"), "my-proxy");

        let rewritten = ModelId::from("grok/grok-4.6").qualify("xai");
        assert_eq!(rewritten.as_str(), "xai/grok-4.6");
    }

    #[test]
    fn request_default_has_empty_collections() {
        let req = Request::default();
        assert_eq!(req.model, ModelId::default());
        assert_eq!(req.system, None);
        assert!(req.messages.is_empty());
        assert!(req.tools.is_empty());
        assert_eq!(req.tool_choice, ToolChoice::Auto);
    }

    #[test]
    fn request_round_trips() {
        let req = Request {
            model: ModelId::from("gpt-4"),
            system: Some("be helpful".to_string()),
            messages: vec![Message::user(vec![ContentBlock::text("hi")])],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.model, req.model);
        assert_eq!(decoded.system, req.system);
        assert_eq!(decoded.tool_choice, req.tool_choice);
        assert_eq!(decoded.messages[0].role, Role::User);
    }

    #[test]
    fn builder_chain_produces_correct_request() {
        let req = Request::builder()
            .model("gpt-4")
            .system("be helpful")
            .message(Message::user(vec![ContentBlock::text("hello")]))
            .message(Message::assistant(vec![ContentBlock::text("hi")]))
            .tool(Tool {
                name: "search".into(),
                description: Some("Search the web".into()),
                input_schema: Arc::new(serde_json::json!({"type": "object"})),
            })
            .tool_choice(ToolChoice::Auto)
            .build()
            .unwrap();

        assert_eq!(req.model.as_str(), "gpt-4");
        assert_eq!(req.system.as_deref(), Some("be helpful"));
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[1].role, Role::Assistant);
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "search");
        assert_eq!(req.tool_choice, ToolChoice::Auto);
    }

    #[test]
    fn builder_errors_when_model_missing() {
        let err = Request::builder().system("be helpful").build().unwrap_err();

        assert_eq!(err, BuilderError::MissingModel);
        assert_eq!(err.to_string(), "builder missing required field: model");
    }

    #[test]
    fn fluent_setters_chain_correctly() {
        let mut req = Request::default();
        req.model("gpt-4")
            .system("be helpful")
            .tool_choice(ToolChoice::Any);

        assert_eq!(req.model.as_str(), "gpt-4");
        assert_eq!(req.system.as_deref(), Some("be helpful"));
        assert_eq!(req.tool_choice, ToolChoice::Any);
    }

    #[test]
    fn message_appends() {
        let mut req = Request::default();
        req.message(Message::user(vec![ContentBlock::text("hello")]))
            .message(Message::assistant(vec![ContentBlock::text("hi")]));

        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[1].role, Role::Assistant);
    }

    #[test]
    fn messages_replaces() {
        let mut req = Request::default();
        req.message(Message::user(vec![ContentBlock::text("old")]))
            .messages(vec![Message::user(vec![ContentBlock::text("new")])]);

        assert_eq!(req.messages.len(), 1);
        // Verify the new content is present (check via serialization roundtrip).
        let json = serde_json::to_value(&req.messages[0]).unwrap();
        assert_eq!(json["content"][0]["text"], "new");
    }

    #[test]
    fn tool_appends() {
        let mut req = Request::default();
        req.tool(Tool {
            name: "a".into(),
            description: None,
            input_schema: Arc::new(serde_json::json!({})),
        })
        .tool(Tool {
            name: "b".into(),
            description: None,
            input_schema: Arc::new(serde_json::json!({})),
        });

        assert_eq!(req.tools.len(), 2);
        assert_eq!(req.tools[0].name, "a");
        assert_eq!(req.tools[1].name, "b");
    }

    #[test]
    fn tools_replaces() {
        let mut req = Request::default();
        req.tool(Tool {
            name: "old".into(),
            description: None,
            input_schema: Arc::new(serde_json::json!({})),
        })
        .tools(vec![Tool {
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
            .model("claude-sonnet-4-20250514")
            .system("You are a coding assistant.")
            .message(Message::user(vec![ContentBlock::text(
                "Write hello world in Rust.",
            )]))
            .tool(Tool {
                name: "run_code".into(),
                description: Some("Execute code".into()),
                input_schema: Arc::new(serde_json::json!({"type": "object"})),
            })
            .build()
            .expect("model id's set");

        // Simulate agent loop mutation
        req.message(Message::assistant(vec![
            ContentBlock::thinking("Let me write code."),
            ContentBlock::text(
                "Here is the code:\n```rust\nfn main() { println!(\"Hello!\"); }\n```",
            ),
        ]))
        .message(Message::user(vec![ContentBlock::text("Run it.")]));

        assert_eq!(req.messages.len(), 3);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[1].role, Role::Assistant);
        assert_eq!(req.messages[2].role, Role::User);
        assert_eq!(req.tools.len(), 1);
    }
}

//! `Role` / `ContentBlock` / `Message`：provider 无关的消息模型。

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 消息角色。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    User,
    System,
    Assistant,
    Tool,
}

/// 统一内容块 —— 参照 Anthropic 的粒度设计，足以表达任意 provider 的消息内容。
///
/// `ToolResult` 统一挂在 `Role::User` 消息下；由各 provider 的 encoder 负责
/// 翻译为其 wire 表示（例如 OpenAI 的 `role: "tool"` 消息）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Thinking {
        thinking: String,
    },
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    ToolUse(ToolUse),
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlock>,
        #[serde(default)]
        is_error: bool,
    },
}

/// 模型发起的一次工具调用。
///
/// `raw_arguments` 是 wire 上原样收到的 `arguments` JSON 文本，逐字节保留。
///
/// provider 的隐式上下文缓存按「完整匹配一个缓存前缀单元」判定命中，其中
/// 「模型输出结束位置」的单元就是模型当时生成的那串 token。重放 assistant 消息
/// 时若重新序列化 `input`（紧凑分隔符、key 按字典序），文本与生成时不一致，
/// 该单元便永远无法完整匹配，整轮（思维链 + 正文 + 工具调用）都会掉出缓存。
/// 所以 decoder 与 `StreamCollector` 把原始文本存在这里，encoder 重放时优先
/// 原样回传。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUse {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) input: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) raw_arguments: Option<String>,
}

/// 工具参数不是合法 JSON 时的错误。
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ToolArgumentsError {
    /// 原文不是合法 JSON。原文本身可以从 [`ToolUse::raw_arguments`] 拿到。
    #[error("tool arguments are not valid JSON")]
    InvalidJson,
}

impl ToolUse {
    /// 一次手工构造的工具调用：没有 wire 原文，重放时用 `input` 的紧凑序列化。
    pub fn new(id: impl Into<String>, name: impl Into<String>, input: serde_json::Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            input,
            raw_arguments: None,
        }
    }

    /// 一次从 wire 解析出来的工具调用。
    ///
    /// `raw` 是模型生成的那串 `arguments` 文本。空白文本没有可回放的原文：
    /// `input` 为 `{}`，原文为 `None`。非法 JSON 同样保留原文，调用方据此把
    /// 这次调用作为工具错误交回模型；`input` 用 `{}`，重放时
    /// [`wire_arguments`](Self::wire_arguments) 会退回它。
    pub fn from_raw(id: impl Into<String>, name: impl Into<String>, raw: String) -> Self {
        let (input, raw_arguments) = split_tool_arguments(raw);
        Self {
            id: id.into(),
            name: name.into(),
            input,
            raw_arguments,
        }
    }

    /// provider 给这次调用分配的 id，回传结果时原样带上。
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// 解析后的参数。参数不是合法 JSON 时这里是空对象：真正要跑的值用
    /// [`arguments`](Self::arguments)，它在这种情况下报错。
    pub fn input(&self) -> &serde_json::Value {
        &self.input
    }

    /// wire 上的原始 `arguments` 文本；手工构造的调用没有。
    pub fn raw_arguments(&self) -> Option<&str> {
        self.raw_arguments.as_deref()
    }

    /// 改写参数，并清掉 wire 原文：原文与新参数不再对应，不能再原样回放。
    pub fn set_input(&mut self, input: serde_json::Value) {
        self.input = input;
        self.raw_arguments = None;
    }

    /// 这次调用要跑的参数。原文不是合法 JSON 时返回错误；调用方据此把这次
    /// 调用作为工具错误交回模型，而不是拿一个空参数去跑。
    pub fn arguments(&self) -> Result<&serde_json::Value, ToolArgumentsError> {
        if self.raw_is_json() {
            Ok(&self.input)
        } else {
            Err(ToolArgumentsError::InvalidJson)
        }
    }

    /// 重放这次调用时 wire 上应该发出的 `arguments` 文本。
    ///
    /// 优先逐字节回传 `raw_arguments`（模型当时生成的那串 JSON），否则退回
    /// `input` 的紧凑序列化——重新序列化会让 provider 的缓存单元失配。残缺
    /// 原文不能原样回放，否则下一次请求会被 provider 拒绝。
    pub fn wire_arguments(&self) -> String {
        match self.raw_arguments.as_deref() {
            Some(raw) if self.arguments().is_ok() => raw.to_owned(),
            _ => self.input.to_string(),
        }
    }

    /// wire 上的原文是不是合法 JSON。必须与 [`from_raw`](Self::from_raw) 用同一个
    /// 解析器，否则 `input` 与 `raw_arguments` 会互相矛盾。
    fn raw_is_json(&self) -> bool {
        match self.raw_arguments.as_deref() {
            Some(raw) => serde_json::from_str::<serde_json::Value>(raw).is_ok(),
            None => true,
        }
    }
}

fn split_tool_arguments(raw: String) -> (serde_json::Value, Option<String>) {
    if raw.trim().is_empty() {
        return (serde_json::json!({}), None);
    }
    let input =
        serde_json::from_str::<serde_json::Value>(&raw).unwrap_or_else(|_| serde_json::json!({}));
    (input, Some(raw))
}

impl ContentBlock {
    /// 便捷构造一个 `ContentBlock::Thinking` 内容块。
    pub fn thinking(thinking: impl Into<String>) -> ContentBlock {
        ContentBlock::Thinking {
            thinking: thinking.into(),
        }
    }

    /// 便捷构造一个 `ContentBlock::Text` 内容块。
    pub fn text(text: impl Into<String>) -> ContentBlock {
        ContentBlock::Text { text: text.into() }
    }

    /// 便捷构造一个不带 wire 原始 `arguments` 的 `ContentBlock::ToolUse`
    /// （`raw_arguments` 为 `None`，encoder 会用 `input` 的紧凑序列化重放）。
    pub fn tool_use(
        id: impl Into<String>,
        name: impl Into<String>,
        input: serde_json::Value,
    ) -> ContentBlock {
        ContentBlock::ToolUse(ToolUse::new(id, name, input))
    }

    /// 这块是不是一次工具调用。
    pub fn as_tool_use(&self) -> Option<&ToolUse> {
        match self {
            ContentBlock::ToolUse(call) => Some(call),
            _ => None,
        }
    }
}

/// 图片来源：内联 base64 数据或远程 URL。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

/// 一条对话消息：角色 + 内容块列表。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// 便捷构造一条 `Role::System` 文本消息。
    pub fn system(text: impl Into<String>) -> Message {
        Message {
            role: Role::System,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// 便捷构造一条 `Role::User` 消息。
    pub fn user(content: impl Into<Vec<ContentBlock>>) -> Message {
        Message {
            role: Role::User,
            content: content.into(),
        }
    }

    /// 便捷构造一条仅含文本的 `Role::User` 消息。
    pub fn user_text(text: impl Into<String>) -> Message {
        Message::user(vec![ContentBlock::text(text)])
    }

    /// 便捷构造一条 `Role::Assistant` 消息。
    pub fn assistant(content: impl Into<Vec<ContentBlock>>) -> Message {
        Message {
            role: Role::Assistant,
            content: content.into(),
        }
    }

    /// 便捷构造一条仅含文本的 `Role::Assistant` 消息。
    pub fn assistant_text(text: impl Into<String>) -> Message {
        Message::assistant(vec![ContentBlock::text(text)])
    }

    /// 便捷构造一条 `Role::Tool` 消息，用于携带工具执行结果。
    pub fn tool(content: impl Into<Vec<ContentBlock>>) -> Message {
        Message {
            role: Role::Tool,
            content: content.into(),
        }
    }

    /// 便捷构造一条仅含单个文本工具结果的 `Role::Tool` 消息。
    pub fn tool_result(id: impl Into<String>, text: impl Into<String>, is_error: bool) -> Message {
        Message::tool(vec![ContentBlock::ToolResult {
            tool_use_id: id.into(),
            content: vec![ContentBlock::text(text)],
            is_error,
        }])
    }

    /// Constructs system prompt from this `Message` only when its role is `Role::System`.
    /// Otherwise, it returns `None`
    pub fn system_prompt(&self) -> Option<String> {
        match self.role {
            Role::System => Some(
                self.content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_default_is_user() {
        assert_eq!(Role::default(), Role::User);
    }

    #[test]
    fn role_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), "\"system\"");
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
        assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), "\"tool\"");
    }

    #[test]
    fn content_block_text_constructor() {
        let block = ContentBlock::text("hello");
        match block {
            ContentBlock::Text { text } => assert_eq!(text, "hello"),
            _ => panic!("expected Text block"),
        }
    }

    #[test]
    fn content_block_thinking_constructor() {
        let block = ContentBlock::thinking("reasoning...");
        match block {
            ContentBlock::Thinking { thinking } => assert_eq!(thinking, "reasoning..."),
            _ => panic!("expected Thinking block"),
        }
    }

    #[test]
    fn content_block_thinking_tag_is_snake_case() {
        let block = ContentBlock::thinking("let me think");
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "thinking");
        assert_eq!(json["thinking"], "let me think");
    }

    #[test]
    fn content_block_tag_is_snake_case() {
        let block = ContentBlock::text("hi");
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "hi");
    }

    #[test]
    fn content_block_image_base64_serializes_correctly() {
        let block = ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: "image/png".to_string(),
                data: "AAAA".to_string(),
            },
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "image");
        assert_eq!(json["source"]["type"], "base64");
        assert_eq!(json["source"]["media_type"], "image/png");
        assert_eq!(json["source"]["data"], "AAAA");
    }

    #[test]
    fn content_block_image_url_serializes_correctly() {
        let block = ContentBlock::Image {
            source: ImageSource::Url {
                url: "https://example.com/a.png".to_string(),
            },
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "image");
        assert_eq!(json["source"]["type"], "url");
        assert_eq!(json["source"]["url"], "https://example.com/a.png");
    }

    #[test]
    fn content_block_tool_use_serializes_correctly() {
        let block = ContentBlock::tool_use(
            "tool_1",
            "get_weather",
            serde_json::json!({ "city": "Beijing" }),
        );
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_use");
        assert_eq!(json["id"], "tool_1");
        assert_eq!(json["name"], "get_weather");
        assert_eq!(json["input"], serde_json::json!({ "city": "Beijing" }));
    }

    #[test]
    fn tool_use_constructor_leaves_raw_arguments_empty() {
        let block = ContentBlock::tool_use("t1", "read_file", serde_json::json!({ "path": "a" }));
        let call = block.as_tool_use().expect("expected ToolUse");
        assert_eq!(call.id(), "t1");
        assert_eq!(call.name(), "read_file");
        assert_eq!(call.raw_arguments(), None);
    }

    /// 重放必须优先原样回传 wire 上的原始文本，否则模型输出边界的缓存单元失配。
    #[test]
    fn tool_arguments_prefers_raw_wire_text() {
        let raw = r#"{"path": "src/main.rs", "limit": 10}"#;
        let call = ToolUse::from_raw("t1", "read_file", raw.to_string());
        assert_eq!(
            call.wire_arguments(),
            raw,
            "raw text must be replayed byte for byte"
        );
        assert_eq!(
            call.input(),
            &serde_json::json!({ "path": "src/main.rs", "limit": 10 })
        );
    }

    /// 没有原始文本时退回紧凑序列化：空格被去掉、key 按字典序重排。
    #[test]
    fn tool_arguments_falls_back_to_compact_serialization() {
        let call = ToolUse::new(
            "t1",
            "read_file",
            serde_json::json!({ "path": "src/main.rs", "limit": 10 }),
        );
        assert_eq!(
            call.wire_arguments(),
            r#"{"limit":10,"path":"src/main.rs"}"#
        );
    }

    /// 残缺的 `arguments` 不能原样回放，否则下一次 provider 请求会因非法 JSON 失败。
    #[test]
    fn tool_arguments_falls_back_when_raw_is_not_json() {
        let call = ToolUse::from_raw("t1", "read_file", "{".to_string());
        assert_eq!(call.wire_arguments(), "{}");
    }

    #[test]
    fn from_raw_keeps_invalid_text_and_empty_input() {
        let call = ToolUse::from_raw("t1", "read_file", "{".to_string());
        assert_eq!(call.input(), &serde_json::json!({}));
        assert_eq!(call.raw_arguments(), Some("{"));
    }

    #[test]
    fn from_raw_drops_blank_text() {
        let call = ToolUse::from_raw("t1", "read_file", "  ".to_string());
        assert_eq!(call.input(), &serde_json::json!({}));
        assert_eq!(call.raw_arguments(), None);
    }

    /// 截断的参数不是工具能跑的值。
    #[test]
    fn arguments_reports_truncated_json() {
        let call = ToolUse::from_raw("t1", "read_file", "{".to_string());
        assert_eq!(call.arguments(), Err(ToolArgumentsError::InvalidJson));
    }

    #[test]
    fn arguments_are_the_parsed_input() {
        let call = ToolUse::from_raw("t1", "read_file", r#"{"path": "src/main.rs"}"#.to_string());
        assert_eq!(call.arguments().unwrap()["path"], "src/main.rs");
    }

    /// 判定参数合法性必须和解析原文用同一个解析器，否则浮点溢出、嵌套过深
    /// 这类原文会被放行。
    #[test]
    fn arguments_and_replay_agree_with_the_parser() {
        let raws = [
            r#"{"path": "src/main.rs"}"#.to_string(),
            "{".to_string(),
            r#"{"x":1e999}"#.to_string(),
            "null".to_string(),
            "[]".to_string(),
            format!("{}{}", "[".repeat(200), "]".repeat(200)),
        ];
        for raw in raws {
            let call = ToolUse::from_raw("t1", "read_file", raw.clone());
            let parses = serde_json::from_str::<serde_json::Value>(&raw).is_ok();
            assert_eq!(call.arguments().is_ok(), parses, "{raw}");
            let replayed = call.wire_arguments();
            assert!(
                serde_json::from_str::<serde_json::Value>(&replayed).is_ok(),
                "{raw} replayed as {replayed}, which the provider would reject"
            );
            if parses {
                assert_eq!(replayed, raw, "valid raw must be replayed byte for byte");
            }
        }
    }

    /// 改了参数就不能再原样回放旧原文。
    #[test]
    fn set_input_drops_the_raw_arguments() {
        let mut call = ToolUse::from_raw("t1", "read_file", r#"{"path": "a"}"#.to_string());
        call.set_input(serde_json::json!({ "path": "b" }));
        assert_eq!(call.raw_arguments(), None);
        assert_eq!(call.wire_arguments(), r#"{"path":"b"}"#);
        assert_eq!(call.arguments().unwrap()["path"], "b");
    }

    #[test]
    fn tool_use_serializes_raw_arguments_only_when_present() {
        let block = ContentBlock::ToolUse(ToolUse::from_raw(
            "t1",
            "read_file",
            r#"{ "path": "a" }"#.to_string(),
        ));
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["raw_arguments"], r#"{ "path": "a" }"#);

        let without = ContentBlock::tool_use("t1", "read_file", serde_json::json!({ "path": "a" }));
        let json = serde_json::to_value(&without).unwrap();
        assert!(json.get("raw_arguments").is_none());
    }

    /// 0.4.2 之前的会话记录里没有 `raw_arguments`，必须仍然能反序列化。
    #[test]
    fn tool_use_deserializes_without_raw_arguments() {
        let json = serde_json::json!({
            "type": "tool_use",
            "id": "t1",
            "name": "read_file",
            "input": { "path": "a" }
        });
        let block: ContentBlock = serde_json::from_value(json).unwrap();
        let call = block.as_tool_use().expect("expected ToolUse");
        assert_eq!(call.raw_arguments(), None);
        assert_eq!(call.id(), "t1");
        assert_eq!(call.name(), "read_file");
    }

    #[test]
    fn tool_use_round_trips_raw_arguments_through_serde() {
        let block = ContentBlock::ToolUse(ToolUse::from_raw(
            "t1",
            "read_file",
            r#"{ "path": "a" }"#.to_string(),
        ));
        let json = serde_json::to_string(&block).unwrap();
        let call = serde_json::from_str::<ContentBlock>(&json)
            .unwrap()
            .as_tool_use()
            .expect("expected ToolUse")
            .clone();
        assert_eq!(call.raw_arguments(), Some(r#"{ "path": "a" }"#));
    }

    #[test]
    fn content_block_tool_result_serializes_correctly() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "tool_1".to_string(),
            content: vec![ContentBlock::text("42 度")],
            is_error: true,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["tool_use_id"], "tool_1");
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][0]["text"], "42 度");
        assert_eq!(json["is_error"], true);
    }

    #[test]
    fn image_source_base64_tag_is_snake_case() {
        let source = ImageSource::Base64 {
            media_type: "image/jpeg".to_string(),
            data: "BBBB".to_string(),
        };
        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json["type"], "base64");
        assert_eq!(json["media_type"], "image/jpeg");
        assert_eq!(json["data"], "BBBB");
    }

    #[test]
    fn image_source_url_tag_is_snake_case() {
        let source = ImageSource::Url {
            url: "https://example.com/b.png".to_string(),
        };
        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json["type"], "url");
        assert_eq!(json["url"], "https://example.com/b.png");
    }

    #[test]
    fn tool_result_defaults_is_error_to_false() {
        let json = serde_json::json!({
            "type": "tool_result",
            "tool_use_id": "id1",
            "content": []
        });
        let block: ContentBlock = serde_json::from_value(json).unwrap();
        match block {
            ContentBlock::ToolResult { is_error, .. } => assert!(!is_error),
            _ => panic!("expected ToolResult block"),
        }
    }

    #[test]
    fn message_system_constructor() {
        let msg = Message::system("be helpful");
        assert_eq!(msg.role, Role::System);
        assert_eq!(msg.system_prompt(), Some("be helpful".into()));
        match &msg.content[..] {
            [ContentBlock::Text { text }] => assert_eq!(text, "be helpful"),
            _ => panic!("expected single text block"),
        }
    }

    #[test]
    fn message_user_constructor() {
        let msg = Message::user(vec![ContentBlock::text("hi")]);
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content.len(), 1);
        assert!(msg.system_prompt().is_none());
    }

    #[test]
    fn message_user_text_constructor() {
        let msg = Message::user_text("hi");
        assert_eq!(msg.role, Role::User);
        assert!(msg.system_prompt().is_none());
        match &msg.content[..] {
            [ContentBlock::Text { text }] => assert_eq!(text, "hi"),
            _ => panic!("expected single text block"),
        }
    }

    #[test]
    fn message_assistant_constructor() {
        let msg = Message::assistant(vec![ContentBlock::text("hi")]);
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content.len(), 1);
        assert!(msg.system_prompt().is_none());
    }

    #[test]
    fn message_assistant_text_constructor() {
        let msg = Message::assistant_text("hi");
        assert_eq!(msg.role, Role::Assistant);
        match &msg.content[..] {
            [ContentBlock::Text { text }] => assert_eq!(text, "hi"),
            _ => panic!("expected single text block"),
        }
        assert!(msg.system_prompt().is_none());
    }

    #[test]
    fn message_tool_constructor() {
        let msg = Message::tool(vec![ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: vec![ContentBlock::text("ok")],
            is_error: false,
        }]);
        assert_eq!(msg.role, Role::Tool);
        assert_eq!(msg.content.len(), 1);
        assert!(msg.system_prompt().is_none());
    }

    #[test]
    fn message_tool_result_constructor() {
        let msg = Message::tool_result("t1", "ok", false);
        assert_eq!(msg.role, Role::Tool);
        match &msg.content[..] {
            [
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                },
            ] => {
                assert_eq!(tool_use_id, "t1");
                assert!(!is_error);
                match &content[..] {
                    [ContentBlock::Text { text }] => assert_eq!(text, "ok"),
                    _ => panic!("expected single text content"),
                }
            }
            _ => panic!("expected single ToolResult block"),
        }
    }

    #[test]
    fn message_default_is_user_with_empty_content() {
        let msg = Message::default();
        assert_eq!(msg.role, Role::User);
        assert!(msg.content.is_empty());
    }
}

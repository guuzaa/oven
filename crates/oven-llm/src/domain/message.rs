//! `Role` / `ContentBlock` / `Message`：provider 无关的消息模型。

use serde::{Deserialize, Serialize};

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
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        /// wire 上原样收到的 `arguments` JSON 文本，逐字节保留。
        ///
        /// provider 的隐式上下文缓存按「完整匹配一个缓存前缀单元」判定命中，
        /// 其中「模型输出结束位置」的单元就是模型当时生成的那串 token。重放
        /// assistant 消息时若重新序列化 `input`（紧凑分隔符、key 按字典序），
        /// 文本与生成时不一致，该单元便永远无法完整匹配，整轮（思维链 + 正文 +
        /// 工具调用）都会掉出缓存。所以 decoder 与 `StreamCollector` 把原始文本
        /// 存放在这里，encoder 重放时优先原样回传。
        ///
        /// `None` 表示这条内容块不是从 wire 解析出来的（手工构造），encoder 会
        /// 退回 `input` 的紧凑序列化。若调用方改写了 `input`，必须同时更新或清空
        /// 本字段，否则重放出去的仍是旧参数。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw_arguments: Option<String>,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlock>,
        #[serde(default)]
        is_error: bool,
    },
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
        ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
            raw_arguments: None,
        }
    }

    /// 重放 `ToolUse` 时 wire 上应该发出的 `arguments` 文本。
    ///
    /// 优先逐字节回传 `raw_arguments`（decoder / `StreamCollector` 从 wire 上
    /// 原样收下的那串文本，等同于模型当时生成的 JSON）；只有没有原始文本时才
    /// 退回 `input` 的紧凑序列化。重新序列化会去掉 `": "` 的空格并按字典序重排
    /// key，使 provider 的缓存单元失配，因此重放路径必须保持字节一致。
    pub fn tool_arguments(input: &serde_json::Value, raw_arguments: Option<&str>) -> String {
        match raw_arguments {
            Some(raw) => raw.to_owned(),
            None => input.to_string(),
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
        let block = ContentBlock::ToolUse {
            id: "tool_1".to_string(),
            name: "get_weather".to_string(),
            input: serde_json::json!({ "city": "Beijing" }),
            raw_arguments: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_use");
        assert_eq!(json["id"], "tool_1");
        assert_eq!(json["name"], "get_weather");
        assert_eq!(json["input"], serde_json::json!({ "city": "Beijing" }));
    }

    #[test]
    fn tool_use_constructor_leaves_raw_arguments_empty() {
        let block = ContentBlock::tool_use("t1", "read_file", serde_json::json!({ "path": "a" }));
        match block {
            ContentBlock::ToolUse {
                id, raw_arguments, ..
            } => {
                assert_eq!(id, "t1");
                assert_eq!(raw_arguments, None);
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    /// 重放必须优先原样回传 wire 上的原始文本，否则模型输出边界的缓存单元失配。
    #[test]
    fn tool_arguments_prefers_raw_wire_text() {
        let input = serde_json::json!({ "path": "src/main.rs", "limit": 10 });
        let raw = r#"{"path": "src/main.rs", "limit": 10}"#;
        assert_eq!(
            ContentBlock::tool_arguments(&input, Some(raw)),
            raw,
            "raw text must be replayed byte for byte"
        );
        // 没有原始文本时退回紧凑序列化：空格被去掉、key 按字典序重排。
        assert_eq!(
            ContentBlock::tool_arguments(&input, None),
            r#"{"limit":10,"path":"src/main.rs"}"#
        );
    }

    #[test]
    fn tool_use_serializes_raw_arguments_only_when_present() {
        let block = ContentBlock::ToolUse {
            id: "t1".to_string(),
            name: "read_file".to_string(),
            input: serde_json::json!({ "path": "a" }),
            raw_arguments: Some(r#"{ "path": "a" }"#.to_string()),
        };
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
        match block {
            ContentBlock::ToolUse { raw_arguments, .. } => assert_eq!(raw_arguments, None),
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn tool_use_round_trips_raw_arguments_through_serde() {
        let block = ContentBlock::ToolUse {
            id: "t1".to_string(),
            name: "read_file".to_string(),
            input: serde_json::json!({ "path": "a" }),
            raw_arguments: Some(r#"{ "path": "a" }"#.to_string()),
        };
        let json = serde_json::to_string(&block).unwrap();
        match serde_json::from_str::<ContentBlock>(&json).unwrap() {
            ContentBlock::ToolUse { raw_arguments, .. } => {
                assert_eq!(raw_arguments.as_deref(), Some(r#"{ "path": "a" }"#));
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
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

//! encoder：将 domain 层的 `Request` 转换为 OpenAI Responses API 的 wire
//! 格式 `CreateResponseRequest`。
//!
//! 本模块是纯函数集合，不做任何 I/O；不读取或处理
//! `Request.provider_options` 字段（该字段的合并逻辑位于 transport 层）。

use thiserror::Error;

use super::types::{
    InputContentPart, InputMessageContent, ResponseInputItem, ResponseRequest, ResponseTool,
    WireReasoning,
};
use crate::domain::message::{ContentBlock, ImageSource, Message, Role};
use crate::domain::request::{ReasoningEffort, Request, ThinkingMode};
use crate::domain::tool::{Tool, ToolChoice};

/// `encode_request` 及其子函数的编码失败原因。
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum EncodeError {
    /// 消息角色与内容块组合不受支持（例如 `Role::System` 消息中出现非
    /// `Text` 块，或 `Role::User` 消息中出现 `ToolUse` 块）。
    #[error("invalid content block: {0}")]
    InvalidContent(String),
    /// `ContentBlock::ToolResult.content` 中出现无法编码的内容块。
    #[error("unsupported tool result content for tool_use_id {0}")]
    UnsupportedToolResultContent(String),
}

/// 将 `Request` 编码为 OpenAI Responses API 的 wire 请求体。
///
/// - `req.system` → 顶层 `instructions`
/// - `messages` 中每条消息按角色分发到 `encode_message`
/// - `tools` / `tool_choice` / `reasoning` 逐一映射
/// - `sampling.stop` 不被 Responses API 支持，忽略
/// - `stream = true` 时 wire 的 `stream` 字段为 `true`
///
/// 不读取或处理 `req.provider_options`（由 transport 层负责合并）。
pub(crate) fn encode_request(req: &Request, stream: bool) -> Result<ResponseRequest, EncodeError> {
    let mut input: Vec<ResponseInputItem> = Vec::new();

    for message in &req.messages {
        encode_message(message, &mut input)?;
    }

    let tools = if req.tools.is_empty() {
        None
    } else {
        Some(req.tools.iter().map(encode_tool).collect())
    };

    Ok(ResponseRequest {
        model: req.model.wire_id().to_owned(),
        instructions: req.system.clone(),
        input: if input.is_empty() { None } else { Some(input) },
        tools,
        tool_choice: encode_tool_choice(&req.tool_choice),
        reasoning: encode_reasoning(req.reasoning_effort, req.thinking),
        temperature: req.sampling.temperature,
        top_p: req.sampling.top_p,
        max_output_tokens: req.sampling.max_tokens,
        stream,
    })
}

/// 按消息角色分发到具体的编码函数，将结果追加到 `out`。
///
/// `Role::User` / `Role::Assistant` / `Role::Tool` 消息可能展开为多个输入项
/// （消息项 + 独立的 `function_call` / `function_call_output` 项），因此该
/// 函数接收 `&mut Vec<ResponseInputItem>` 而非返回单个输入项。
fn encode_message(message: &Message, out: &mut Vec<ResponseInputItem>) -> Result<(), EncodeError> {
    match message.role {
        Role::System => out.push(encode_system_message(message)?),
        Role::User => encode_user_message(message, out)?,
        Role::Assistant => encode_assistant_message(message, out)?,
        Role::Tool => encode_tool_message(message, out)?,
    }
    Ok(())
}

/// 编码一条 `Role::System` 消息：仅允许 `Text` 内容块（拼接为单一文本），
/// 产出 `{"type":"message","role":"system","content":"..."}` 输入项；其余
/// 内容块返回 `EncodeError::InvalidContent`。
fn encode_system_message(message: &Message) -> Result<ResponseInputItem, EncodeError> {
    let mut text = String::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text: t } => text.push_str(t),
            other => {
                return Err(EncodeError::InvalidContent(format!(
                    "system message may only contain text blocks, found {other:?}"
                )));
            }
        }
    }
    Ok(ResponseInputItem::Message {
        role: "system".to_string(),
        content: InputMessageContent::Text(text),
    })
}

/// 编码一条 `Role::User` 消息：按内容块出现顺序，将连续的 `Text`/`Image`
/// 块合并为 message 输入项，将 `ToolResult` 块展开为独立的
/// `function_call_output` 输入项，保持相对顺序。
///
/// `Thinking` / `ToolUse` 块不允许出现在 `Role::User` 消息中，返回
/// `EncodeError::InvalidContent`。
fn encode_user_message(
    message: &Message,
    out: &mut Vec<ResponseInputItem>,
) -> Result<(), EncodeError> {
    let mut pending: Vec<&ContentBlock> = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Text { .. } | ContentBlock::Image { .. } => {
                pending.push(block);
            }
            ContentBlock::Thinking { .. } => {
                return Err(EncodeError::InvalidContent(
                    "Thinking block is not allowed in a Role::User message".to_string(),
                ));
            }
            ContentBlock::ToolUse { .. } => {
                return Err(EncodeError::InvalidContent(
                    "ToolUse block is not allowed in a Role::User message".to_string(),
                ));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                flush_user_parts(&mut pending, out);
                out.push(encode_tool_result(tool_use_id, content, *is_error)?);
            }
        }
    }

    flush_user_parts(&mut pending, out);
    Ok(())
}

/// 将累积的连续 `Text`/`Image` 内容块合并为一个 message 输入项并追加到
/// `out`；若没有累积任何块则不产生输入项。清空 `pending`。
///
/// 纯文本（单个 `Text` 块）时 `content` 用字符串表示；多部分（多个文本块、
/// 或含图片）时用 `input_text` / `input_image` 内容部分数组表示。Base64
/// 图片编码为 data URL，URL 图片直接透传。
fn flush_user_parts(pending: &mut Vec<&ContentBlock>, out: &mut Vec<ResponseInputItem>) {
    if pending.is_empty() {
        return;
    }

    let content = match pending[..] {
        [ContentBlock::Text { text }] => InputMessageContent::Text(text.clone()),
        _ => {
            let parts = pending
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => {
                        InputContentPart::InputText { text: text.clone() }
                    }
                    ContentBlock::Image { source } => InputContentPart::InputImage {
                        image_url: encode_image_url(source),
                    },
                    _ => unreachable!("pending only contains Text/Image blocks"),
                })
                .collect();
            InputMessageContent::Parts(parts)
        }
    };

    out.push(ResponseInputItem::Message {
        role: "user".to_string(),
        content,
    });
    pending.clear();
}

/// 将 `ImageSource` 编码为 Responses API 的 `image_url` 字符串：Base64 图片
/// 编码为 `data:<media_type>;base64,<data>`，URL 图片直接透传。
fn encode_image_url(source: &ImageSource) -> String {
    match source {
        ImageSource::Base64 { media_type, data } => {
            format!("data:{media_type};base64,{data}")
        }
        ImageSource::Url { url } => url.clone(),
    }
}

/// 编码一个 `ContentBlock::ToolResult` 为 `function_call_output` 输入项。
///
/// `output` 为拼接文本：`Text` 块拼接其文本，其余内容块 JSON 序列化后拼接；
/// `is_error` 为 `true` 时在文本前加上 `"Error: "` 前缀。
fn encode_tool_result(
    tool_use_id: &str,
    content: &[ContentBlock],
    is_error: bool,
) -> Result<ResponseInputItem, EncodeError> {
    let mut output = String::new();
    for block in content {
        match block {
            ContentBlock::Text { text } => output.push_str(text),
            other => {
                let serialized = serde_json::to_string(other).map_err(|_| {
                    EncodeError::UnsupportedToolResultContent(tool_use_id.to_string())
                })?;
                output.push_str(&serialized);
            }
        }
    }

    if is_error {
        output = format!("Error: {output}");
    }

    Ok(ResponseInputItem::FunctionCallOutput {
        call_id: tool_use_id.to_string(),
        output,
    })
}

/// 编码一条 `Role::Tool` 消息：将每个 `ToolResult` 块展开为独立的
/// `function_call_output` 输入项，保持原始顺序。仅允许 `ToolResult` 内容块。
fn encode_tool_message(
    message: &Message,
    out: &mut Vec<ResponseInputItem>,
) -> Result<(), EncodeError> {
    for block in &message.content {
        match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                out.push(encode_tool_result(tool_use_id, content, *is_error)?);
            }
            other => {
                return Err(EncodeError::InvalidContent(format!(
                    "Role::Tool message may only contain ToolResult blocks, found {other:?}"
                )));
            }
        }
    }
    Ok(())
}

/// 编码一条 `Role::Assistant` 消息：将连续的 `Text` 块合并为一个 message
/// 输入项（`output_text` 内容部分），将 `ToolUse` 块转换为独立的
/// `function_call` 输入项，保持相对顺序。`Thinking` 块 v1 不回传、直接丢弃。
///
/// `Image` / `ToolResult` 块返回 `EncodeError::InvalidContent`。
fn encode_assistant_message(
    message: &Message,
    out: &mut Vec<ResponseInputItem>,
) -> Result<(), EncodeError> {
    let mut text_parts: Vec<String> = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Thinking { .. } => { /* 不回传思维内容 */ }
            ContentBlock::Text { text } => text_parts.push(text.clone()),
            ContentBlock::ToolUse {
                id,
                name,
                input,
                raw_arguments,
            } => {
                flush_assistant_text(&mut text_parts, out);
                out.push(ResponseInputItem::FunctionCall {
                    call_id: id.clone(),
                    name: name.clone(),
                    // 优先原样回传 wire 原始文本，见 [`ContentBlock::tool_arguments`]。
                    arguments: ContentBlock::tool_arguments(input, raw_arguments.as_deref()),
                });
            }
            other => {
                return Err(EncodeError::InvalidContent(format!(
                    "assistant message may only contain text, thinking and tool_use blocks, found {other:?}"
                )));
            }
        }
    }

    flush_assistant_text(&mut text_parts, out);
    Ok(())
}

/// 将累积的 assistant 文本部分合并为一个 message 输入项（每个 `Text` 块
/// 对应一个 `output_text` 内容部分）并追加到 `out`；无累积时不产生输入项。
fn flush_assistant_text(text_parts: &mut Vec<String>, out: &mut Vec<ResponseInputItem>) {
    if text_parts.is_empty() {
        return;
    }

    let parts = text_parts
        .drain(..)
        .map(|text| InputContentPart::OutputText { text })
        .collect();
    out.push(ResponseInputItem::Message {
        role: "assistant".to_string(),
        content: InputMessageContent::Parts(parts),
    });
}

/// 编码一个 `Tool` 定义为 wire 格式的 `{"type":"function","name":...,
/// "parameters":{...}}`。工具 schema 键统一使用 `parameters`。
fn encode_tool(tool: &Tool) -> ResponseTool {
    ResponseTool {
        kind: "function".to_string(),
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.input_schema.clone(),
    }
}

/// 编码 `ToolChoice` 为 wire 格式：`Auto→省略`、`None→"none"`、
/// `Any→"required"`、`Tool(name)→{"type":"function","name":name}`。
fn encode_tool_choice(choice: &ToolChoice) -> Option<serde_json::Value> {
    match choice {
        ToolChoice::Auto => None,
        ToolChoice::Any => Some(serde_json::json!("required")),
        ToolChoice::None => Some(serde_json::json!("none")),
        ToolChoice::Tool(name) => Some(serde_json::json!({
            "type": "function",
            "name": name
        })),
    }
}

/// 编码 `reasoning` 参数：
///
/// - 有 `reasoning_effort` 时按枚举输出对应档位（`none`/`low`/`medium`/
///   `high`）；
/// - 无 `reasoning_effort` 但 `thinking == Disabled` 时输出
///   `{"effort":"none"}`；
/// - 其余情况（`thinking == Enabled` 或未设置）不输出 `reasoning` 字段。
fn encode_reasoning(
    reasoning_effort: Option<ReasoningEffort>,
    thinking: Option<ThinkingMode>,
) -> Option<WireReasoning> {
    let effort = match reasoning_effort {
        Some(ReasoningEffort::None) => Some("none"),
        Some(ReasoningEffort::Low) => Some("low"),
        Some(ReasoningEffort::Medium) => Some("medium"),
        Some(ReasoningEffort::High) => Some("high"),
        None if thinking == Some(ThinkingMode::Disabled) => Some("none"),
        None => None,
    };

    effort.map(|effort| WireReasoning {
        effort: effort.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::request::SamplingParams;

    // --- 纯文本请求 ---

    #[test]
    fn system_field_becomes_instructions() {
        let req = Request {
            system: Some("be helpful".to_string()),
            messages: vec![Message::user_text("hi")],
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(wire.instructions.as_deref(), Some("be helpful"));
        assert_eq!(wire.input.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn user_text_message_encodes_as_string_content() {
        let req = Request {
            messages: vec![Message::user_text("hello")],
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        let input = wire.input.unwrap();
        assert_eq!(input.len(), 1);
        match &input[0] {
            ResponseInputItem::Message { role, content } => {
                assert_eq!(role, "user");
                assert_eq!(*content, InputMessageContent::Text("hello".to_string()));
            }
            other => panic!("expected Message input item, got {other:?}"),
        }
    }

    #[test]
    fn empty_messages_omit_input() {
        let req = Request::default();
        let wire = encode_request(&req, false).unwrap();
        assert!(wire.input.is_none());
        assert!(wire.instructions.is_none());
    }

    // --- 多轮：assistant 文本 + ToolUse、ToolResult ---

    /// 重放 `function_call` 时同样原样回传 wire 原始 `arguments`，否则
    /// provider 的模型输出边界缓存单元失配。
    #[test]
    fn encode_replays_raw_function_call_arguments() {
        let raw = r#"{ "city": "Hangzhou" }"#;
        let req = Request {
            messages: vec![Message::assistant(vec![ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "get_weather".to_string(),
                input: serde_json::from_str(raw).unwrap(),
                raw_arguments: Some(raw.to_string()),
            }])],
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        let input = wire.input.unwrap();
        match &input[0] {
            ResponseInputItem::FunctionCall { arguments, .. } => assert_eq!(arguments, raw),
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn multi_turn_encodes_items_in_order() {
        let req = Request {
            messages: vec![
                Message::user_text("what's the weather?"),
                Message::assistant(vec![
                    ContentBlock::thinking("let me check"),
                    ContentBlock::text("I'll check."),
                    ContentBlock::tool_use(
                        "call_1".to_string(),
                        "get_weather".to_string(),
                        serde_json::json!({"city": "Hangzhou"}),
                    ),
                ]),
                Message::tool_result("call_1", "sunny", false),
            ],
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        let input = wire.input.unwrap();
        assert_eq!(input.len(), 4);

        // 1. user message
        assert!(
            matches!(&input[0], ResponseInputItem::Message { role, content }
            if role == "user" && *content == InputMessageContent::Text("what's the weather?".to_string()))
        );

        // 2. assistant message（Thinking 丢弃，文本 → output_text parts）
        match &input[1] {
            ResponseInputItem::Message { role, content } => {
                assert_eq!(role, "assistant");
                match content {
                    InputMessageContent::Parts(parts) => {
                        assert_eq!(parts.len(), 1);
                        assert_eq!(
                            parts[0],
                            InputContentPart::OutputText {
                                text: "I'll check.".to_string()
                            }
                        );
                    }
                    other => panic!("expected Parts content, got {other:?}"),
                }
            }
            other => panic!("expected assistant Message item, got {other:?}"),
        }

        // 3. assistant function_call
        match &input[2] {
            ResponseInputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                assert_eq!(call_id, "call_1");
                assert_eq!(name, "get_weather");
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(arguments).unwrap(),
                    serde_json::json!({"city": "Hangzhou"})
                );
            }
            other => panic!("expected FunctionCall item, got {other:?}"),
        }

        // 4. function_call_output
        match &input[3] {
            ResponseInputItem::FunctionCallOutput { call_id, output } => {
                assert_eq!(call_id, "call_1");
                assert_eq!(output, "sunny");
            }
            other => panic!("expected FunctionCallOutput item, got {other:?}"),
        }
    }

    #[test]
    fn assistant_thinking_blocks_are_dropped() {
        let message = Message::assistant(vec![
            ContentBlock::thinking("hidden reasoning"),
            ContentBlock::text("answer"),
        ]);
        let mut out = Vec::new();
        encode_assistant_message(&message, &mut out).unwrap();
        assert_eq!(out.len(), 1);
        match &out[0] {
            ResponseInputItem::Message { content, .. } => {
                let serialized = serde_json::to_string(content).unwrap();
                assert!(!serialized.contains("hidden reasoning"));
            }
            other => panic!("expected Message item, got {other:?}"),
        }
    }

    // --- Role::System 消息 ---

    #[test]
    fn system_message_encodes_as_system_input_item() {
        let req = Request {
            messages: vec![Message::system("be terse")],
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        let input = wire.input.unwrap();
        match &input[0] {
            ResponseInputItem::Message { role, content } => {
                assert_eq!(role, "system");
                assert_eq!(*content, InputMessageContent::Text("be terse".to_string()));
            }
            other => panic!("expected Message item, got {other:?}"),
        }
    }

    #[test]
    fn system_message_with_non_text_block_errors() {
        let message = Message {
            role: Role::System,
            content: vec![ContentBlock::tool_use(
                "id1".to_string(),
                "f".to_string(),
                serde_json::json!({}),
            )],
        };
        let err = encode_system_message(&message).unwrap_err();
        assert!(matches!(err, EncodeError::InvalidContent(_)));
    }

    // --- 用户消息：多部分与图片 ---

    #[test]
    fn user_text_and_image_become_parts() {
        use crate::domain::message::ImageSource;
        let message = Message::user(vec![
            ContentBlock::text("what is this?"),
            ContentBlock::Image {
                source: ImageSource::Base64 {
                    media_type: "image/png".to_string(),
                    data: "AAAA".to_string(),
                },
            },
        ]);
        let mut out = Vec::new();
        encode_user_message(&message, &mut out).unwrap();
        assert_eq!(out.len(), 1);
        match &out[0] {
            ResponseInputItem::Message { content, .. } => match content {
                InputMessageContent::Parts(parts) => {
                    assert_eq!(parts.len(), 2);
                    assert_eq!(
                        parts[0],
                        InputContentPart::InputText {
                            text: "what is this?".to_string()
                        }
                    );
                    assert_eq!(
                        parts[1],
                        InputContentPart::InputImage {
                            image_url: "data:image/png;base64,AAAA".to_string()
                        }
                    );
                }
                other => panic!("expected Parts content, got {other:?}"),
            },
            other => panic!("expected Message item, got {other:?}"),
        }
    }

    #[test]
    fn user_url_image_passes_through() {
        use crate::domain::message::ImageSource;
        let message = Message::user(vec![ContentBlock::Image {
            source: ImageSource::Url {
                url: "https://example.com/a.png".to_string(),
            },
        }]);
        let mut out = Vec::new();
        encode_user_message(&message, &mut out).unwrap();
        match &out[0] {
            ResponseInputItem::Message { content, .. } => match content {
                InputMessageContent::Parts(parts) => {
                    assert_eq!(
                        parts[0],
                        InputContentPart::InputImage {
                            image_url: "https://example.com/a.png".to_string()
                        }
                    );
                }
                other => panic!("expected Parts content, got {other:?}"),
            },
            other => panic!("expected Message item, got {other:?}"),
        }
    }

    #[test]
    fn user_tool_result_expands_to_function_call_output() {
        let message = Message::user(vec![
            ContentBlock::text("here is the result:"),
            ContentBlock::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: vec![ContentBlock::text("42")],
                is_error: false,
            },
        ]);
        let mut out = Vec::new();
        encode_user_message(&message, &mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[0], ResponseInputItem::Message { .. }));
        match &out[1] {
            ResponseInputItem::FunctionCallOutput { call_id, output } => {
                assert_eq!(call_id, "call_1");
                assert_eq!(output, "42");
            }
            other => panic!("expected FunctionCallOutput item, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_error_prefixes_output() {
        let message = Message::tool_result("call_1", "boom", true);
        let mut out = Vec::new();
        encode_tool_message(&message, &mut out).unwrap();
        match &out[0] {
            ResponseInputItem::FunctionCallOutput { output, .. } => {
                assert_eq!(output, "Error: boom");
            }
            other => panic!("expected FunctionCallOutput item, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_non_text_block_is_json_serialized() {
        let message = Message::tool(vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".to_string(),
            content: vec![ContentBlock::tool_use(
                "inner".to_string(),
                "nested".to_string(),
                serde_json::json!({"a": 1}),
            )],
            is_error: false,
        }]);
        let mut out = Vec::new();
        encode_tool_message(&message, &mut out).unwrap();
        match &out[0] {
            ResponseInputItem::FunctionCallOutput { output, .. } => {
                let parsed: serde_json::Value = serde_json::from_str(output).unwrap();
                assert_eq!(parsed["name"], "nested");
                assert_eq!(parsed["input"]["a"], 1);
            }
            other => panic!("expected FunctionCallOutput item, got {other:?}"),
        }
    }

    #[test]
    fn user_message_with_tool_use_errors() {
        let message = Message::user(vec![ContentBlock::tool_use(
            "id1".to_string(),
            "f".to_string(),
            serde_json::json!({}),
        )]);
        let mut out = Vec::new();
        let err = encode_user_message(&message, &mut out).unwrap_err();
        assert!(matches!(err, EncodeError::InvalidContent(_)));
    }

    // --- tools / tool_choice ---

    #[test]
    fn tools_encode_with_parameters_key() {
        let req = Request {
            tools: vec![Tool {
                name: "get_weather".to_string(),
                description: Some("query weather".to_string()),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        let tools = wire.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].kind, "function");
        assert_eq!(tools[0].name, "get_weather");
        assert_eq!(tools[0].description.as_deref(), Some("query weather"));
        assert_eq!(tools[0].parameters, serde_json::json!({"type": "object"}));

        let json = serde_json::to_value(&tools[0]).unwrap();
        assert!(json.get("parameters").is_some());
        assert!(json.get("input_schema").is_none());
    }

    #[test]
    fn tool_choice_auto_is_omitted() {
        let req = Request {
            tool_choice: ToolChoice::Auto,
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert!(wire.tool_choice.is_none());
    }

    #[test]
    fn tool_choice_none_is_none_string() {
        let req = Request {
            tool_choice: ToolChoice::None,
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(wire.tool_choice, Some(serde_json::json!("none")));
    }

    #[test]
    fn tool_choice_any_is_required_string() {
        let req = Request {
            tool_choice: ToolChoice::Any,
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(wire.tool_choice, Some(serde_json::json!("required")));
    }

    #[test]
    fn tool_choice_tool_is_function_object() {
        let req = Request {
            tool_choice: ToolChoice::Tool("get_weather".to_string()),
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(
            wire.tool_choice,
            Some(serde_json::json!({"type": "function", "name": "get_weather"}))
        );
    }

    // --- reasoning / thinking ---

    #[test]
    fn reasoning_effort_levels_map_to_wire() {
        for (effort, expected) in [
            (ReasoningEffort::None, "none"),
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
        ] {
            let req = Request {
                reasoning_effort: Some(effort),
                ..Default::default()
            };
            let wire = encode_request(&req, false).unwrap();
            assert_eq!(wire.reasoning.unwrap().effort, expected);
        }
    }

    #[test]
    fn thinking_disabled_maps_to_reasoning_none() {
        let req = Request {
            thinking: Some(ThinkingMode::Disabled),
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(wire.reasoning.unwrap().effort, "none");
    }

    #[test]
    fn reasoning_effort_wins_over_thinking_disabled() {
        let req = Request {
            thinking: Some(ThinkingMode::Disabled),
            reasoning_effort: Some(ReasoningEffort::High),
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(wire.reasoning.unwrap().effort, "high");
    }

    #[test]
    fn thinking_enabled_without_effort_omits_reasoning() {
        let req = Request {
            thinking: Some(ThinkingMode::Enabled),
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert!(wire.reasoning.is_none());
    }

    #[test]
    fn no_reasoning_fields_omit_reasoning() {
        let req = Request::default();
        let wire = encode_request(&req, false).unwrap();
        assert!(wire.reasoning.is_none());
    }

    // --- sampling ---

    #[test]
    fn sampling_maps_temperature_top_p_max_output_tokens() {
        let req = Request {
            sampling: SamplingParams {
                temperature: Some(0.5),
                top_p: Some(0.9),
                max_tokens: Some(100),
                stop: Some(vec!["STOP".to_string()]),
            },
            ..Default::default()
        };
        let wire = encode_request(&req, true).unwrap();
        assert_eq!(wire.temperature, Some(0.5));
        assert_eq!(wire.top_p, Some(0.9));
        assert_eq!(wire.max_output_tokens, Some(100));
        assert!(wire.stream);

        let json = serde_json::to_value(&wire).unwrap();
        assert!(
            json.get("stop").is_none(),
            "stop is not supported and must be ignored"
        );
        assert_eq!(json["stream"], true);
    }

    #[test]
    fn stream_false_by_default() {
        let req = Request::default();
        let wire = encode_request(&req, false).unwrap();
        assert!(!wire.stream);
    }

    #[test]
    fn assistant_message_with_image_errors() {
        use crate::domain::message::ImageSource;
        let message = Message::assistant(vec![ContentBlock::Image {
            source: ImageSource::Url {
                url: "https://example.com/a.png".to_string(),
            },
        }]);
        let mut out = Vec::new();
        let err = encode_assistant_message(&message, &mut out).unwrap_err();
        assert!(matches!(err, EncodeError::InvalidContent(_)));
    }
}

//! encoder：将 domain 层的 `Request` 转换为 OpenAI Chat Completions 的
//! wire 格式 `ChatCompletionRequest`。
//!
//! 本模块是纯函数集合，不做任何 I/O；不读取或处理
//! `Request.provider_options` 字段（该字段的合并逻辑位于 transport 层）。

#![allow(dead_code)]

use thiserror::Error;

use super::types::{
    ChatCompletionRequest, StopValue, StreamOptions, WireFunction, WireMessage, WireThinking,
    WireTool, WireToolCall, WireToolCallFunction,
};
use crate::domain::message::{ContentBlock, Message, Role};
use crate::domain::request::Request;
use crate::domain::tool::{Tool, ToolChoice};

/// `encode_request` 及其子函数的编码失败原因。
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum CompletionsEncodeError {
    /// 消息角色与内容块组合不受支持（例如 `Role::System` 消息中出现非
    /// `Text` 块，或 `Role::User` 消息中出现 `ToolUse` 块）。
    #[error("invalid content block: {0}")]
    InvalidContent(String),
    /// `ContentBlock::ToolResult.content` 中出现非 `Text` 块。OpenAI 兼容
    /// 模式仅支持文本工具结果。
    #[error("unsupported tool result content for tool_use_id {0}")]
    UnsupportedToolResultContent(String),
}

/// 将 `Request` 编码为 OpenAI Chat Completions 的 wire 请求体。
///
/// - `system` 字段（如果有值）生成消息列表的第一条 `role: "system"` 消息
/// - `messages` 中每条消息按角色分发给 `encode_message`
/// - `tools` / `tool_choice` / `sampling.stop` 逐一映射
/// - `stream = true` 时附带 `stream_options.include_usage = true`
///
/// 不读取或处理 `req.provider_options`（由 transport 层负责合并）。
pub(crate) fn encode_request(
    req: &Request,
    stream: bool,
) -> Result<ChatCompletionRequest, CompletionsEncodeError> {
    let mut messages = Vec::new();

    if let Some(system) = &req.system {
        messages.push(WireMessage {
            role: "system".to_string(),
            content: Some(system.clone()),
            ..Default::default()
        });
    }

    for message in &req.messages {
        encode_message(message, &mut messages)?;
    }

    let tools = if req.tools.is_empty() {
        None
    } else {
        Some(req.tools.iter().map(encode_tool).collect())
    };

    let stop = match &req.sampling.stop {
        None => None,
        Some(values) if values.is_empty() => None,
        Some(values) if values.len() == 1 => Some(StopValue::Single(values[0].clone())),
        Some(values) => Some(StopValue::Multiple(values.clone())),
    };

    let stream_options = if stream {
        Some(StreamOptions {
            include_usage: true,
        })
    } else {
        None
    };

    let thinking = req.thinking.map(|t| WireThinking {
        mode: t.to_string(),
    });

    let reasoning_effort = req.reasoning_effort.map(|e| e.to_string());

    Ok(ChatCompletionRequest {
        model: req.model.wire_id().to_owned(),
        messages,
        tools,
        tool_choice: Some(encode_tool_choice(&req.tool_choice)),
        temperature: req.sampling.temperature,
        top_p: req.sampling.top_p,
        max_tokens: req.sampling.max_tokens,
        stop,
        thinking,
        reasoning_effort,
        stream,
        stream_options,
    })
}

/// 按消息角色分发到具体的编码函数，将结果追加到 `out`。
///
/// `Role::User` 消息可能展开为多条 wire 消息（普通内容 + 若干独立的
/// `tool` 消息），因此该函数接收 `&mut Vec<WireMessage>` 而非返回单条消息。
fn encode_message(
    message: &Message,
    out: &mut Vec<WireMessage>,
) -> Result<(), CompletionsEncodeError> {
    match message.role {
        Role::System => out.push(encode_system_message(message)?),
        Role::User => encode_user_message(message, out)?,
        Role::Assistant => out.push(encode_assistant_message(message)?),
        Role::Tool => encode_tool_message(message, out)?,
    }
    Ok(())
}

/// 编码一条 `Role::System` 消息：仅允许 `Text` 内容块，其余内容块返回
/// `EncodeError::InvalidContent`。
fn encode_system_message(message: &Message) -> Result<WireMessage, CompletionsEncodeError> {
    let mut text = String::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text: t } => text.push_str(t),
            other => {
                return Err(CompletionsEncodeError::InvalidContent(format!(
                    "system message may only contain text blocks, found {other:?}"
                )));
            }
        }
    }
    Ok(WireMessage {
        role: "system".to_string(),
        content: Some(text),
        ..Default::default()
    })
}

/// 编码一条 `Role::User` 消息：按内容块出现顺序，将连续的 `Text`/`Image`
/// 块合并为一条 `user` wire 消息，将 `ToolResult` 块展开为独立的
/// `role: "tool"` wire 消息，保持与其他内容块的相对顺序。
///
/// `ToolUse` 块不允许出现在 `Role::User` 消息中，返回
/// `EncodeError::InvalidContent`。
fn encode_user_message(
    message: &Message,
    out: &mut Vec<WireMessage>,
) -> Result<(), CompletionsEncodeError> {
    let mut pending: Vec<&ContentBlock> = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Text { .. } | ContentBlock::Image { .. } => {
                pending.push(block);
            }
            ContentBlock::Thinking { .. } => {
                return Err(CompletionsEncodeError::InvalidContent(
                    "Thinking block is not allowed in a Role::User message".to_string(),
                ));
            }
            ContentBlock::ToolUse { .. } => {
                return Err(CompletionsEncodeError::InvalidContent(
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

/// 将累积的连续 `Text`/`Image` 内容块合并为一条 `role: "user"` wire 消息并
/// 追加到 `out`；若没有累积任何块则不产生消息。清空 `pending`。
///
/// `Image` 块目前不会向 wire 消息的文本 `content` 贡献任何内容（当前
/// `WireMessage.content` 仅支持纯文本，多模态 wire 表示留作后续扩展），但仍
/// 参与合并分组，保证文本与图片混合的内容不会被错误拆分成多条消息。
fn flush_user_parts(pending: &mut Vec<&ContentBlock>, out: &mut Vec<WireMessage>) {
    if pending.is_empty() {
        return;
    }

    let mut text = String::new();
    for block in pending.iter() {
        if let ContentBlock::Text { text: t } = block {
            text.push_str(t);
        }
    }

    out.push(WireMessage {
        role: "user".to_string(),
        content: Some(text),
        ..Default::default()
    });
    pending.clear();
}

/// 编码一个 `ContentBlock::ToolResult` 为一条独立的 `role: "tool"` wire
/// 消息。仅允许 `Text` 内容块，其余返回
/// `EncodeError::UnsupportedToolResultContent`；`is_error` 为 `true` 时在
/// 文本前加上 `"Error: "` 前缀。
fn encode_tool_result(
    tool_use_id: &str,
    content: &[ContentBlock],
    is_error: bool,
) -> Result<WireMessage, CompletionsEncodeError> {
    let mut text = String::new();
    for block in content {
        match block {
            ContentBlock::Text { text: t } => text.push_str(t),
            _ => {
                return Err(CompletionsEncodeError::UnsupportedToolResultContent(
                    tool_use_id.to_string(),
                ));
            }
        }
    }

    if is_error {
        text = format!("Error: {text}");
    }

    Ok(WireMessage {
        role: "tool".to_string(),
        content: Some(text),
        tool_call_id: Some(tool_use_id.to_string()),
        ..Default::default()
    })
}

/// 编码一条 `Role::Tool` 消息：将每个 `ToolResult` 块展开为独立的
/// `role: "tool"` wire 消息，保持原始顺序。仅允许 `ToolResult` 内容块。
fn encode_tool_message(
    message: &Message,
    out: &mut Vec<WireMessage>,
) -> Result<(), CompletionsEncodeError> {
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
                return Err(CompletionsEncodeError::InvalidContent(format!(
                    "Role::Tool message may only contain ToolResult blocks, found {other:?}"
                )));
            }
        }
    }
    Ok(())
}

/// 编码一条 `Role::Assistant` 消息：合并 `Text` 块为 `content`，合并
/// `Thinking` 块为 `reasoning_content`（交错式思考需要回传思考内容以保证
/// 多轮上下文一致），将 `ToolUse` 块转换为 `tool_calls`；其余内容块
/// （`Image`、`ToolResult`）返回 `EncodeError::InvalidContent`。
///
/// `tool_calls[].function.arguments` 优先原样回传内容块里的 wire 原始文本
/// （见 [`ContentBlock::tool_arguments`]）：provider 的隐式上下文缓存按「完整
/// 匹配缓存单元」判定，重新序列化会让模型输出边界的单元失配。
fn encode_assistant_message(message: &Message) -> Result<WireMessage, CompletionsEncodeError> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Thinking { thinking } => reasoning.push_str(thinking),
            ContentBlock::Text { text: t } => text.push_str(t),
            ContentBlock::ToolUse {
                id,
                name,
                input,
                raw_arguments,
            } => {
                tool_calls.push(WireToolCall {
                    id: id.clone(),
                    kind: "function".to_string(),
                    function: WireToolCallFunction {
                        name: name.clone(),
                        arguments: ContentBlock::tool_arguments(input, raw_arguments.as_deref()),
                    },
                });
            }
            other => {
                return Err(CompletionsEncodeError::InvalidContent(format!(
                    "assistant message may only contain text and tool_use blocks, found {other:?}"
                )));
            }
        }
    }

    Ok(WireMessage {
        role: "assistant".to_string(),
        content: if text.is_empty() { None } else { Some(text) },
        reasoning_content: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        ..Default::default()
    })
}

/// 编码一个 `Tool` 定义为 wire 格式的 `{"type": "function", "function": {...}}`。
fn encode_tool(tool: &Tool) -> WireTool {
    WireTool {
        kind: "function".to_string(),
        function: WireFunction {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.input_schema.clone(),
        },
    }
}

/// 编码 `ToolChoice` 为 wire 格式：
/// `Auto→"auto"`、`Any→"required"`、`None→"none"`、
/// `Tool(name)→{"type":"function","function":{"name":name}}`。
fn encode_tool_choice(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Auto => serde_json::json!("auto"),
        ToolChoice::Any => serde_json::json!("required"),
        ToolChoice::None => serde_json::json!("none"),
        ToolChoice::Tool(name) => serde_json::json!({
            "type": "function",
            "function": { "name": name }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::request::{ReasoningEffort, SamplingParams, ThinkingMode};

    // --- encode_request: system field (Requirement 3.1) ---

    #[test]
    fn system_field_generates_first_system_message() {
        let req = Request {
            system: Some("be helpful".to_string()),
            messages: vec![Message::user(vec![ContentBlock::text("hi")])],
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(wire.messages[0].role, "system");
        assert_eq!(wire.messages[0].content, Some("be helpful".to_string()));
        assert_eq!(wire.messages[1].role, "user");
    }

    #[test]
    fn no_system_field_produces_no_system_message() {
        let req = Request {
            messages: vec![Message::user(vec![ContentBlock::text("hi")])],
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(wire.messages.len(), 1);
        assert_eq!(wire.messages[0].role, "user");
    }

    // --- encode_system_message (Requirement 3.4) ---

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
        assert!(matches!(err, CompletionsEncodeError::InvalidContent(_)));
    }

    #[test]
    fn system_message_concatenates_multiple_text_blocks() {
        let message = Message {
            role: Role::System,
            content: vec![ContentBlock::text("a"), ContentBlock::text("b")],
        };
        let wire = encode_system_message(&message).unwrap();
        assert_eq!(wire.content, Some("ab".to_string()));
    }

    // --- sampling.stop mapping (Requirement 3.6) ---

    #[test]
    fn stop_single_value_serializes_as_single() {
        let req = Request {
            sampling: SamplingParams {
                stop: Some(vec!["STOP".to_string()]),
                ..Default::default()
            },
            ..Default::default()
        };
        let wire = encode_request(&req, true).unwrap();
        assert_eq!(wire.stop, Some(StopValue::Single("STOP".to_string())));
    }

    #[test]
    fn stop_multiple_values_serializes_as_multiple() {
        let req = Request {
            sampling: SamplingParams {
                stop: Some(vec!["A".to_string(), "B".to_string()]),
                ..Default::default()
            },
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(
            wire.stop,
            Some(StopValue::Multiple(vec!["A".to_string(), "B".to_string()]))
        );
    }

    #[test]
    fn stop_none_maps_to_none() {
        let req = Request::default();
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(wire.stop, None);
    }

    #[test]
    fn stop_empty_vec_maps_to_none() {
        let req = Request {
            sampling: SamplingParams {
                stop: Some(vec![]),
                ..Default::default()
            },
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(wire.stop, None);
    }

    // --- encode_user_message / ToolResult expansion (Requirement 3.2) ---

    #[test]
    fn tool_result_expands_to_independent_tool_message() {
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
        assert_eq!(out[0].role, "user");
        assert_eq!(out[0].content, Some("here is the result:".to_string()));
        assert_eq!(out[1].role, "tool");
        assert_eq!(out[1].tool_call_id, Some("call_1".to_string()));
        assert_eq!(out[1].content, Some("42".to_string()));
    }

    #[test]
    fn tool_result_preserves_relative_order_with_other_blocks() {
        let message = Message::user(vec![
            ContentBlock::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: vec![ContentBlock::text("first")],
                is_error: false,
            },
            ContentBlock::text("middle"),
            ContentBlock::ToolResult {
                tool_use_id: "call_2".to_string(),
                content: vec![ContentBlock::text("second")],
                is_error: false,
            },
        ]);
        let mut out = Vec::new();
        encode_user_message(&message, &mut out).unwrap();
        let tool_call_ids: Vec<Option<String>> =
            out.iter().map(|m| m.tool_call_id.clone()).collect();
        assert_eq!(
            tool_call_ids,
            vec![Some("call_1".to_string()), None, Some("call_2".to_string())]
        );
        assert_eq!(out[1].role, "user");
        assert_eq!(out[1].content, Some("middle".to_string()));
    }

    #[test]
    fn tool_result_is_error_prefixes_error_text() {
        let message = Message::user(vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".to_string(),
            content: vec![ContentBlock::text("boom")],
            is_error: true,
        }]);
        let mut out = Vec::new();
        encode_user_message(&message, &mut out).unwrap();
        assert_eq!(out[0].content, Some("Error: boom".to_string()));
    }

    #[test]
    fn tool_result_with_non_text_content_errors() {
        let message = Message::user(vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".to_string(),
            content: vec![ContentBlock::tool_use(
                "id1".to_string(),
                "f".to_string(),
                serde_json::json!({}),
            )],
            is_error: false,
        }]);
        let mut out = Vec::new();
        let err = encode_user_message(&message, &mut out).unwrap_err();
        assert_eq!(
            err,
            CompletionsEncodeError::UnsupportedToolResultContent("call_1".to_string())
        );
    }

    #[test]
    fn tool_use_in_user_message_errors() {
        let message = Message::user(vec![ContentBlock::tool_use(
            "id1".to_string(),
            "f".to_string(),
            serde_json::json!({}),
        )]);
        let mut out = Vec::new();
        let err = encode_user_message(&message, &mut out).unwrap_err();
        assert!(matches!(err, CompletionsEncodeError::InvalidContent(_)));
    }

    #[test]
    fn user_message_with_only_tool_result_has_no_extra_empty_user_message() {
        let message = Message::user(vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".to_string(),
            content: vec![ContentBlock::text("ok")],
            is_error: false,
        }]);
        let mut out = Vec::new();
        encode_user_message(&message, &mut out).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, "tool");
    }

    // --- encode_assistant_message / ToolUse -> tool_calls (Requirement 3.3) ---

    #[test]
    fn assistant_text_merges_into_content() {
        let message = Message::assistant(vec![ContentBlock::text("a"), ContentBlock::text("b")]);
        let wire = encode_assistant_message(&message).unwrap();
        assert_eq!(wire.content, Some("ab".to_string()));
        assert_eq!(wire.reasoning_content, None);
        assert_eq!(wire.tool_calls, None);
    }

    #[test]
    fn assistant_thinking_passes_through_to_reasoning_content() {
        let message = Message::assistant(vec![ContentBlock::thinking("let me think")]);
        let wire = encode_assistant_message(&message).unwrap();
        assert_eq!(wire.content, None);
        assert_eq!(wire.reasoning_content, Some("let me think".to_string()));
        assert_eq!(wire.tool_calls, None);
    }

    #[test]
    fn assistant_interleaved_thinking_and_text_concatenate() {
        let message = Message::assistant(vec![
            ContentBlock::thinking("step one"),
            ContentBlock::text("answer one"),
            ContentBlock::thinking("step two"),
            ContentBlock::text("answer two"),
        ]);
        let wire = encode_assistant_message(&message).unwrap();
        assert_eq!(wire.content, Some("answer oneanswer two".to_string()));
        assert_eq!(wire.reasoning_content, Some("step onestep two".to_string()));
    }

    #[test]
    fn assistant_no_thinking_omits_reasoning_content() {
        let message = Message::assistant(vec![ContentBlock::text("hi")]);
        let wire = encode_assistant_message(&message).unwrap();
        assert_eq!(wire.reasoning_content, None);
    }

    #[test]
    fn assistant_replays_raw_tool_arguments_byte_for_byte() {
        // deepseek 生成 `{"path": "src/main.rs", "limit": 10}`：冒号后有空格、
        // key 是 schema 顺序。重放必须原样回传，重新序列化会让 provider 的
        // 「模型输出结束位置」缓存单元失配。
        let raw = r#"{"path": "src/main.rs", "limit": 10}"#;
        let message = Message::assistant(vec![ContentBlock::ToolUse {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            input: serde_json::from_str(raw).unwrap(),
            raw_arguments: Some(raw.to_string()),
        }]);
        let wire = encode_assistant_message(&message).unwrap();
        let tool_calls = wire.tool_calls.unwrap();
        assert_eq!(tool_calls[0].function.arguments, raw);
    }

    #[test]
    fn assistant_without_raw_arguments_falls_back_to_compact_json() {
        let message = Message::assistant(vec![ContentBlock::tool_use(
            "call_1",
            "read_file",
            serde_json::json!({"path": "src/main.rs", "limit": 10}),
        )]);
        let wire = encode_assistant_message(&message).unwrap();
        let tool_calls = wire.tool_calls.unwrap();
        assert_eq!(
            tool_calls[0].function.arguments,
            r#"{"limit":10,"path":"src/main.rs"}"#
        );
    }

    #[test]
    fn assistant_tool_use_converts_to_tool_calls() {
        let message = Message::assistant(vec![
            ContentBlock::tool_use(
                "call_1".to_string(),
                "get_weather".to_string(),
                serde_json::json!({"city": "Beijing"}),
            ),
            ContentBlock::tool_use(
                "call_2".to_string(),
                "get_time".to_string(),
                serde_json::json!({}),
            ),
        ]);
        let wire = encode_assistant_message(&message).unwrap();
        assert_eq!(wire.content, None);
        let tool_calls = wire.tool_calls.unwrap();
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].function.name, "get_weather");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&tool_calls[0].function.arguments).unwrap(),
            serde_json::json!({"city": "Beijing"})
        );
        assert_eq!(tool_calls[1].id, "call_2");
    }

    #[test]
    fn assistant_message_with_image_errors() {
        use crate::domain::message::ImageSource;
        let message = Message::assistant(vec![ContentBlock::Image {
            source: ImageSource::Url {
                url: "https://example.com/a.png".to_string(),
            },
        }]);
        let err = encode_assistant_message(&message).unwrap_err();
        assert!(matches!(err, CompletionsEncodeError::InvalidContent(_)));
    }

    #[test]
    fn assistant_message_with_tool_result_errors() {
        let message = Message::assistant(vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".to_string(),
            content: vec![ContentBlock::text("x")],
            is_error: false,
        }]);
        let err = encode_assistant_message(&message).unwrap_err();
        assert!(matches!(err, CompletionsEncodeError::InvalidContent(_)));
    }

    // --- encode_tool_message (Role::Tool) ---

    #[test]
    fn tool_message_expands_each_tool_result() {
        let message = Message::tool(vec![
            ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: vec![ContentBlock::text("result 1")],
                is_error: false,
            },
            ContentBlock::ToolResult {
                tool_use_id: "call_2".into(),
                content: vec![ContentBlock::text("result 2")],
                is_error: true,
            },
        ]);
        let mut out = Vec::new();
        encode_tool_message(&message, &mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].role, "tool");
        assert_eq!(out[0].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(out[0].content, Some("result 1".into()));
        assert_eq!(out[1].role, "tool");
        assert_eq!(out[1].tool_call_id.as_deref(), Some("call_2"));
        assert_eq!(out[1].content, Some("Error: result 2".into()));
    }

    #[test]
    fn tool_message_non_tool_result_errors() {
        let message = Message::tool(vec![ContentBlock::text("oops")]);
        let mut out = Vec::new();
        let err = encode_tool_message(&message, &mut out).unwrap_err();
        assert!(matches!(err, CompletionsEncodeError::InvalidContent(_)));
    }

    // --- encode_tool_choice (Requirement 3.7) ---

    #[test]
    fn tool_choice_auto_maps_to_auto_string() {
        assert_eq!(
            encode_tool_choice(&ToolChoice::Auto),
            serde_json::json!("auto")
        );
    }

    #[test]
    fn tool_choice_any_maps_to_required_string() {
        assert_eq!(
            encode_tool_choice(&ToolChoice::Any),
            serde_json::json!("required")
        );
    }

    #[test]
    fn tool_choice_none_maps_to_none_string() {
        assert_eq!(
            encode_tool_choice(&ToolChoice::None),
            serde_json::json!("none")
        );
    }

    #[test]
    fn tool_choice_tool_maps_to_function_object() {
        let value = encode_tool_choice(&ToolChoice::Tool("get_weather".to_string()));
        assert_eq!(
            value,
            serde_json::json!({"type": "function", "function": {"name": "get_weather"}})
        );
    }

    // --- encode_tool ---

    #[test]
    fn encode_tool_maps_fields() {
        let tool = Tool {
            name: "get_weather".to_string(),
            description: Some("query weather".to_string()),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let wire = encode_tool(&tool);
        assert_eq!(wire.kind, "function");
        assert_eq!(wire.function.name, "get_weather");
        assert_eq!(wire.function.description, Some("query weather".to_string()));
        assert_eq!(
            wire.function.parameters,
            serde_json::json!({"type": "object"})
        );
    }

    #[test]
    fn encode_request_maps_tools_and_sampling() {
        let req = Request {
            model: crate::domain::ModelId::from("gpt-4"),
            tools: vec![Tool {
                name: "get_weather".to_string(),
                description: None,
                input_schema: serde_json::json!({}),
            }],
            sampling: SamplingParams {
                temperature: Some(0.5),
                top_p: Some(0.9),
                max_tokens: Some(100),
                stop: None,
            },
            ..Default::default()
        };
        let wire = encode_request(&req, true).unwrap();
        assert_eq!(wire.model, "gpt-4");
    }

    #[test]
    fn encode_request_sends_wire_id_not_slug() {
        let req = Request {
            model: crate::domain::ModelId::from("deepseek/deepseek-v4-flash:responses"),
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(wire.model, "deepseek-v4-flash");
    }

    #[test]
    fn encode_request_maps_rest() {
        let req = Request {
            model: crate::domain::ModelId::from("gpt-4"),
            tools: vec![Tool {
                name: "get_weather".to_string(),
                description: None,
                input_schema: serde_json::json!({}),
            }],
            sampling: SamplingParams {
                temperature: Some(0.5),
                top_p: Some(0.9),
                max_tokens: Some(100),
                stop: None,
            },
            ..Default::default()
        };
        let wire = encode_request(&req, true).unwrap();
        assert_eq!(wire.model, "gpt-4");
        assert_eq!(wire.tools.unwrap().len(), 1);
        assert_eq!(wire.temperature, Some(0.5));
        assert_eq!(wire.top_p, Some(0.9));
        assert_eq!(wire.max_tokens, Some(100));
        assert!(wire.stream);
        assert_eq!(
            wire.stream_options,
            Some(StreamOptions {
                include_usage: true
            })
        );
    }

    #[test]
    fn encode_request_no_stream_has_no_stream_options() {
        let req = Request::default();
        let wire = encode_request(&req, false).unwrap();
        assert!(!wire.stream);
        assert_eq!(wire.stream_options, None);
    }

    #[test]
    fn encode_request_thinking_enabled() {
        let req = Request {
            thinking: Some(ThinkingMode::Enabled),
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        let thinking = wire.thinking.unwrap();
        assert_eq!(thinking.mode, "enabled");
    }

    #[test]
    fn encode_request_thinking_disabled() {
        let req = Request {
            thinking: Some(ThinkingMode::Disabled),
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        let thinking = wire.thinking.unwrap();
        assert_eq!(thinking.mode, "disabled");
    }

    #[test]
    fn encode_request_thinking_none_is_absent() {
        let req = Request::default();
        let wire = encode_request(&req, false).unwrap();
        assert!(wire.thinking.is_none());
    }

    #[test]
    fn encode_request_reasoning_effort_high() {
        let req = Request {
            reasoning_effort: Some(ReasoningEffort::High),
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(wire.reasoning_effort, Some("high".to_string()));
    }

    #[test]
    fn encode_request_reasoning_effort_none_is_absent() {
        let req = Request::default();
        let wire = encode_request(&req, false).unwrap();
        assert!(wire.reasoning_effort.is_none());
    }

    #[test]
    fn encode_request_all_reasoning_effort_levels() {
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
            assert_eq!(wire.reasoning_effort, Some(expected.to_string()));
        }
    }

    #[test]
    fn encode_request_thinking_and_reasoning_effort_together() {
        let req = Request {
            thinking: Some(ThinkingMode::Enabled),
            reasoning_effort: Some(ReasoningEffort::Medium),
            ..Default::default()
        };
        let wire = encode_request(&req, false).unwrap();
        assert_eq!(wire.thinking.unwrap().mode, "enabled");
        assert_eq!(wire.reasoning_effort, Some("medium".to_string()));
    }
}

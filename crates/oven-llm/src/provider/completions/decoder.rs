//! decoder：将 OpenAI Chat Completions 的 wire 格式响应转换为 domain 层的
//! `Response` / `StreamEvent`。

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use thiserror::Error;

use super::types::{
    ChatCompletionChunk, ChatCompletionResponse, WireResponseToolCall, WireStreamDelta,
    WireStreamToolCall, WireUsage,
};
use crate::domain::message::{ContentBlock, Role};
use crate::domain::response::{Response, StopReason, Usage};
use crate::domain::stream::{Delta, StreamEvent, StreamPhase};

/// `decode_response`（及后续流式 `StreamDecoder`）的解码失败原因。
#[derive(Debug, Error)]
pub enum CompletionsDecodeError {
    /// wire 响应的 `choices` 字段为空（Requirement 4.1）。
    #[error("response has no choices")]
    MissingChoice,
    /// wire 响应中 choice 的消息角色不是 `"assistant"`（Requirement 4.3）。
    #[error("unexpected message role: {role}")]
    UnexpectedRole { role: String },
    /// 某个工具调用的 `arguments` 字段不是合法 JSON（Requirement 4.4）。
    #[error("invalid tool arguments JSON for tool call {id}: {source}")]
    InvalidToolArguments {
        id: String,
        #[source]
        source: serde_json::Error,
    },
    /// 通用 JSON 反序列化错误（例如 SSE chunk 解析失败）。
    #[error("JSON deserialization error: {0}")]
    Json(#[source] serde_json::Error),
    /// 流式状态机在 `AwaitingDone` 之后收到了包含非空 `choices` 的 chunk
    /// （Requirement 5.5）。
    #[error("received data after finish_reason")]
    DataAfterFinish,
    /// 流式状态机在 `Stopped` 之后又被调用（`decode_chunk` 或 `finish`）
    /// （Requirement 5.7）。
    #[error("received data after stream stopped")]
    DataAfterStop,
}

/// 将 OpenAI 的 `finish_reason` 字符串映射为 domain 层的 `StopReason`。
///
/// `"stop"` → `EndTurn`，`"tool_calls"` → `ToolUse`，`"length"` →
/// `MaxTokens`，其余任意取值（包括 OpenAI 特有的 `"content_filter"` 等）
/// → `None`（Requirement 4.5）。
pub(crate) fn map_stop_reason(finish_reason: &str) -> Option<StopReason> {
    match finish_reason {
        "stop" => Some(StopReason::EndTurn),
        "tool_calls" => Some(StopReason::ToolUse),
        "length" => Some(StopReason::MaxTokens),
        _ => None,
    }
}

/// 将 OpenAI Chat Completions 的非流式响应体解码为 domain 层的 `Response`。
///
/// - `choices` 为空 → `DecodeError::MissingChoice`
/// - `choices` 多于一个时只取第一个，忽略多余的 choice（与流式行为一致）
/// - 唯一 choice 的消息角色非 `"assistant"` → `DecodeError::UnexpectedRole`
/// - 任意 `tool_calls[].function.arguments` 非合法 JSON →
///   `DecodeError::InvalidToolArguments`
/// - `usage` 字段（若存在）转换为 `Usage { input_tokens, output_tokens }`
/// - `finish_reason`（若存在）通过 `map_stop_reason` 映射为 `StopReason`
pub(crate) fn decode_response(
    wire: ChatCompletionResponse,
) -> Result<Response, CompletionsDecodeError> {
    if wire.choices.is_empty() {
        return Err(CompletionsDecodeError::MissingChoice);
    }

    // 只处理第一个 choice，忽略多余的 choice。
    let choice = wire
        .choices
        .into_iter()
        .next()
        .expect("checked non-empty above");

    if choice.message.role != "assistant" {
        return Err(CompletionsDecodeError::UnexpectedRole {
            role: choice.message.role,
        });
    }

    let mut content = Vec::new();

    if let Some(thinking) = choice.message.reasoning_content
        && !thinking.is_empty()
    {
        content.push(ContentBlock::Thinking { thinking });
    }

    if let Some(text) = choice.message.content {
        content.push(ContentBlock::Text { text });
    }

    if let Some(tool_calls) = choice.message.tool_calls {
        for tool_call in tool_calls {
            content.push(decode_tool_call(tool_call)?);
        }
    }

    let usage = wire.usage.map(decode_usage);
    let stop_reason = choice.finish_reason.as_deref().and_then(map_stop_reason);

    Ok(Response {
        id: wire.id,
        model: wire.model,
        role: Role::Assistant,
        content,
        stop_reason,
        usage,
    })
}

/// 将一个响应侧的 `WireResponseToolCall` 解码为 `ContentBlock::ToolUse`，
/// 解析 `arguments` JSON 字符串失败时返回
/// `DecodeError::InvalidToolArguments`。
///
/// wire 上的 `arguments` 原文会逐字节存进 `raw_arguments`：重放该 assistant
/// 消息时 encoder 原样回传，provider 的隐式上下文缓存（按完整匹配缓存单元
/// 判定）才能命中模型输出边界的单元。
fn decode_tool_call(
    tool_call: WireResponseToolCall,
) -> Result<ContentBlock, CompletionsDecodeError> {
    let input: serde_json::Value =
        serde_json::from_str(&tool_call.function.arguments).map_err(|source| {
            CompletionsDecodeError::InvalidToolArguments {
                id: tool_call.id.clone(),
                source,
            }
        })?;

    Ok(ContentBlock::ToolUse {
        id: tool_call.id,
        name: tool_call.function.name,
        input,
        raw_arguments: Some(tool_call.function.arguments),
    })
}

/// 将 `WireUsage` 转换为 domain 层的 `Usage`。
fn decode_usage(usage: WireUsage) -> Usage {
    // 1. `prompt_tokens_details.cached_tokens`（OpenAI / zhipu / deepseek / kimi）
    // 2. `cached_tokens`（kimi 顶层冗余字段）
    // 3. `prompt_cache_hit_tokens`（deepseek 冗余字段）
    let cache_read_tokens = usage
        .prompt_tokens_details
        .and_then(|d| d.cached_tokens)
        .filter(|v| *v > 0)
        .or_else(|| usage.cached_tokens.filter(|v| *v > 0))
        .or_else(|| usage.prompt_cache_hit_tokens.filter(|v| *v > 0))
        .unwrap_or(0);

    Usage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        cache_read_tokens,
        reasoning_tokens: usage
            .completion_tokens_details
            .and_then(|d| d.reasoning_tokens)
            .or_else(|| usage.prompt_tokens_details.and_then(|d| d.reasoning_tokens))
            .unwrap_or(0),
    }
}

// ---------------------------------------------------------------------------
// 流式 StreamDecoder
// ---------------------------------------------------------------------------

/// 将 OpenAI Chat Completions 流式响应的扁平 delta 升维为 Anthropic 风格的
/// 块事件序列（`ContentBlockStart → ContentBlockDelta* → ContentBlockStop`）。
///
/// 参见设计文档 "decoder：wire → domain（`decoder.rs`）" 一节与需求文档
/// Requirement 5。状态机转换：`Initial → Streaming → AwaitingDone →
/// Stopped`。
#[derive(Debug, Default)]
pub(crate) struct StreamDecoder {
    phase: StreamPhase,
    /// OpenAI 的工具调用 `index` → 本解码器分配的内容块索引。
    tool_block_map: HashMap<u32, usize>,
    /// 当前仍处于「已开启但未关闭」状态的内容块索引集合。
    open_blocks: HashSet<usize>,
    /// 下一个待分配的内容块索引。
    next_block_index: usize,
    /// 文本块（若已开启）对应的内容块索引；文本块全局只会开启一次。
    text_block_index: Option<usize>,
    /// 思维块（若已开启）对应的内容块索引；思维块全局只会开启一次。
    thinking_block_index: Option<usize>,
    /// `finish_reason` 映射出的 `StopReason`。`include_usage` 的用量在
    /// 其后的空 `choices` chunk 才到达，那时要再发一条 `MessageDelta`，
    /// 并带上同一个 `stop_reason`（collector 会整段覆盖）。
    stop_reason: Option<StopReason>,
}

impl StreamDecoder {
    /// 构造一个处于 `Initial` 阶段的新解码器。
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 处理一个 SSE chunk，返回本次调用产出的事件列表（可能为空）。
    ///
    /// - `Stopped` 阶段调用 → `DecodeError::DataAfterStop`
    /// - `AwaitingDone` 阶段收到非空 `choices` → `DecodeError::DataAfterFinish`
    /// - `AwaitingDone` 阶段收到空 `choices` 且带 `usage` → 再产出一条
    ///   `MessageDelta`（`stop_reason` 与 `finish_reason` chunk 相同）。
    ///   `stream_options.include_usage` 的用量在 `finish_reason` 之后单独到达，
    ///   各 provider 都不把它填进 `finish_reason` chunk
    /// - `AwaitingDone` 阶段收到空 `choices` 且无 `usage` → 无操作
    /// - `Initial` 阶段的首个 chunk 会先产出 `MessageStart`，再转入
    ///   `Streaming`
    pub(crate) fn decode_chunk(
        &mut self,
        chunk: ChatCompletionChunk,
    ) -> Result<Vec<StreamEvent>, CompletionsDecodeError> {
        if self.phase == StreamPhase::Stopped {
            return Err(CompletionsDecodeError::DataAfterStop);
        }

        if self.phase == StreamPhase::AwaitingDone {
            if !chunk.choices.is_empty() {
                return Err(CompletionsDecodeError::DataAfterFinish);
            }
            return Ok(match chunk.usage.map(decode_usage) {
                Some(usage) => vec![StreamEvent::MessageDelta {
                    stop_reason: self.stop_reason,
                    usage: Some(usage),
                }],
                None => Vec::new(),
            });
        }

        let mut events = Vec::new();

        if self.phase == StreamPhase::Initial {
            events.push(StreamEvent::MessageStart {
                id: chunk.id.clone(),
                model: chunk.model.clone(),
            });
            self.phase = StreamPhase::Streaming;
        }

        // 与非流式一致：只处理第一个 choice，忽略多余的 choice。
        let Some(choice) = chunk.choices.into_iter().next() else {
            return Ok(events);
        };

        self.decode_delta(&choice.delta, &mut events);

        if let Some(finish_reason) = choice.finish_reason {
            self.close_open_blocks(&mut events);
            let stop_reason = map_stop_reason(&finish_reason);
            self.stop_reason = stop_reason;
            events.push(StreamEvent::MessageDelta {
                stop_reason,
                usage: chunk.usage.map(decode_usage),
            });
            self.phase = StreamPhase::AwaitingDone;
        }

        Ok(events)
    }

    /// 结束流式解码。
    ///
    /// - `Initial` 阶段（从未收到任何 chunk）→ `DecodeError::MissingChoice`
    /// - `Stopped` 阶段 → `DecodeError::DataAfterStop`
    /// - `AwaitingDone` 阶段：块已在 `finish_reason` 到达时关闭，
    ///   `MessageDelta` 也已发出，此处只需产出 `MessageStop`
    /// - `Streaming` 阶段（流结束前从未收到 `finish_reason`）：补齐所有未
    ///   关闭块的 `ContentBlockStop`、一个 `stop_reason`/`usage` 均为 `None`
    ///   的 `MessageDelta`，再产出 `MessageStop`
    pub(crate) fn finish(&mut self) -> Result<Vec<StreamEvent>, CompletionsDecodeError> {
        match self.phase {
            StreamPhase::Initial => Err(CompletionsDecodeError::MissingChoice),
            StreamPhase::Stopped => Err(CompletionsDecodeError::DataAfterStop),
            StreamPhase::AwaitingDone => {
                self.phase = StreamPhase::Stopped;
                Ok(vec![StreamEvent::MessageStop])
            }
            StreamPhase::Streaming => {
                let mut events = Vec::new();
                self.close_open_blocks(&mut events);
                events.push(StreamEvent::MessageDelta {
                    stop_reason: None,
                    usage: None,
                });
                events.push(StreamEvent::MessageStop);
                self.phase = StreamPhase::Stopped;
                Ok(events)
            }
        }
    }

    /// 处理单个 choice 的 delta：分派文本内容、思维内容与工具调用增量。
    fn decode_delta(&mut self, delta: &WireStreamDelta, events: &mut Vec<StreamEvent>) {
        if let Some(thinking) = &delta.reasoning_content {
            let index = self.ensure_thinking_block(events);
            if !thinking.is_empty() {
                events.push(StreamEvent::ContentBlockDelta {
                    index,
                    delta: Delta::ThinkingDelta {
                        thinking: thinking.clone(),
                    },
                });
            }
        }

        if let Some(text) = &delta.content {
            let index = self.ensure_text_block(events);
            if !text.is_empty() {
                events.push(StreamEvent::ContentBlockDelta {
                    index,
                    delta: Delta::TextDelta { text: text.clone() },
                });
            }
        }

        if let Some(tool_calls) = &delta.tool_calls {
            for tool_call in tool_calls {
                self.decode_tool_call_delta(tool_call, events);
            }
        }
    }

    /// 首次出现文本 delta 时开启文本块并返回其索引；已开启时直接返回既有
    /// 索引（文本块全局只开启一次，Requirement 5.2）。
    fn ensure_text_block(&mut self, events: &mut Vec<StreamEvent>) -> usize {
        if let Some(index) = self.text_block_index {
            return index;
        }

        let index = self.allocate_block_index();
        self.text_block_index = Some(index);
        self.open_blocks.insert(index);
        events.push(StreamEvent::ContentBlockStart {
            index,
            block: ContentBlock::Text {
                text: String::new(),
            },
        });
        index
    }

    /// 首次出现思维 delta 时开启思维块并返回其索引；已开启时直接返回既有
    /// 索引（思维块全局只开启一次）。
    fn ensure_thinking_block(&mut self, events: &mut Vec<StreamEvent>) -> usize {
        if let Some(index) = self.thinking_block_index {
            return index;
        }

        let index = self.allocate_block_index();
        self.thinking_block_index = Some(index);
        self.open_blocks.insert(index);
        events.push(StreamEvent::ContentBlockStart {
            index,
            block: ContentBlock::Thinking {
                thinking: String::new(),
            },
        });
        index
    }

    /// 处理单个工具调用增量：首次出现某个 `index` 时开启对应块，随后的
    /// `arguments` 片段以 `ContentBlockDelta` 产出（Requirement 5.3）。
    fn decode_tool_call_delta(
        &mut self,
        tool_call: &WireStreamToolCall,
        events: &mut Vec<StreamEvent>,
    ) {
        let index = self.ensure_tool_block(
            tool_call.index,
            tool_call.id.as_deref(),
            tool_call.function.as_ref().and_then(|f| f.name.as_deref()),
            events,
        );

        if let Some(arguments) = tool_call
            .function
            .as_ref()
            .and_then(|f| f.arguments.as_deref())
            && !arguments.is_empty()
        {
            events.push(StreamEvent::ContentBlockDelta {
                index,
                delta: Delta::InputJsonDelta {
                    partial_json: arguments.to_string(),
                },
            });
        }
    }

    /// 查找或分配 `wire_index` 对应的内容块索引；首次分配时产出
    /// `ContentBlockStart` 事件。
    ///
    /// `input` 使用空字符串 `serde_json::Value::String(String::new())` 作为
    /// 占位：流式阶段 `arguments` 只是逐步拼接的原始字符串片段，尚不构成
    /// 合法 JSON，无法解析为最终的参数对象；真正的参数值由上层在收集完所
    /// 有 `InputJsonDelta` 片段后自行拼接解析。
    fn ensure_tool_block(
        &mut self,
        wire_index: u32,
        id: Option<&str>,
        name: Option<&str>,
        events: &mut Vec<StreamEvent>,
    ) -> usize {
        if let Some(&index) = self.tool_block_map.get(&wire_index) {
            return index;
        }

        let index = self.allocate_block_index();
        self.tool_block_map.insert(wire_index, index);
        self.open_blocks.insert(index);
        events.push(StreamEvent::ContentBlockStart {
            index,
            block: ContentBlock::ToolUse {
                id: id.unwrap_or_default().to_string(),
                name: name.unwrap_or_default().to_string(),
                input: serde_json::Value::String(String::new()),
                raw_arguments: None,
            },
        });
        index
    }

    /// 为所有仍处于 `open_blocks` 中的内容块索引产出 `ContentBlockStop`，
    /// 并清空 `open_blocks`。
    fn close_open_blocks(&mut self, events: &mut Vec<StreamEvent>) {
        let mut indices: Vec<usize> = self.open_blocks.iter().copied().collect();
        indices.sort_unstable();
        for index in indices {
            events.push(StreamEvent::ContentBlockStop { index });
        }
        self.open_blocks.clear();
    }

    /// 分配并递增下一个内容块索引。
    fn allocate_block_index(&mut self) -> usize {
        let index = self.next_block_index;
        self.next_block_index += 1;
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::completions::types::{
        WireChoice, WireResponseMessage, WireResponseToolCallFunction, WireTokenDetails,
    };

    fn choice(message: WireResponseMessage, finish_reason: Option<&str>) -> WireChoice {
        WireChoice {
            index: 0,
            message,
            finish_reason: finish_reason.map(|s| s.to_string()),
        }
    }

    fn assistant_message(content: Option<&str>) -> WireResponseMessage {
        WireResponseMessage {
            role: "assistant".to_string(),
            content: content.map(|s| s.to_string()),
            reasoning_content: None,
            tool_calls: None,
        }
    }

    fn wire_response(choices: Vec<WireChoice>, usage: Option<WireUsage>) -> ChatCompletionResponse {
        ChatCompletionResponse {
            id: "chatcmpl-1".to_string(),
            model: "gpt-4".to_string(),
            choices,
            usage,
        }
    }

    // --- choices 数量处理 ---

    #[test]
    fn empty_choices_returns_missing_choice() {
        let wire = wire_response(vec![], None);
        let err = decode_response(wire).unwrap_err();
        assert!(matches!(err, CompletionsDecodeError::MissingChoice));
    }

    #[test]
    fn multiple_choices_uses_first_choice_and_ignores_rest() {
        let wire = wire_response(
            vec![
                choice(assistant_message(Some("first")), Some("stop")),
                choice(assistant_message(Some("second")), Some("length")),
            ],
            None,
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(response.content.len(), 1);
        match &response.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "first"),
            other => panic!("expected Text block, got {other:?}"),
        }
        // 第二个 choice 的文本与 finish_reason 均被忽略。
        assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
    }

    // --- role check (Requirement 4.3) ---

    #[test]
    fn non_assistant_role_returns_unexpected_role() {
        let message = WireResponseMessage {
            role: "system".to_string(),
            content: Some("hi".to_string()),
            ..Default::default()
        };
        let wire = wire_response(vec![choice(message, Some("stop"))], None);
        let err = decode_response(wire).unwrap_err();
        match err {
            CompletionsDecodeError::UnexpectedRole { role } => assert_eq!(role, "system"),
            other => panic!("expected UnexpectedRole, got {other:?}"),
        }
    }

    // --- invalid tool arguments (Requirement 4.4) ---

    #[test]
    fn invalid_tool_arguments_json_returns_invalid_tool_arguments() {
        let message = WireResponseMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![WireResponseToolCall {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: WireResponseToolCallFunction {
                    name: "get_weather".to_string(),
                    arguments: "not json".to_string(),
                },
            }]),
            ..Default::default()
        };
        let wire = wire_response(vec![choice(message, Some("tool_calls"))], None);
        let err = decode_response(wire).unwrap_err();
        match err {
            CompletionsDecodeError::InvalidToolArguments { id, .. } => assert_eq!(id, "call_1"),
            other => panic!("expected InvalidToolArguments, got {other:?}"),
        }
    }

    // --- finish_reason mapping (Requirement 4.5) ---

    #[test]
    fn map_stop_reason_stop_maps_to_end_turn() {
        assert_eq!(map_stop_reason("stop"), Some(StopReason::EndTurn));
    }

    #[test]
    fn map_stop_reason_tool_calls_maps_to_tool_use() {
        assert_eq!(map_stop_reason("tool_calls"), Some(StopReason::ToolUse));
    }

    #[test]
    fn map_stop_reason_length_maps_to_max_tokens() {
        assert_eq!(map_stop_reason("length"), Some(StopReason::MaxTokens));
    }

    #[test]
    fn map_stop_reason_other_maps_to_none() {
        assert_eq!(map_stop_reason("content_filter"), None);
        assert_eq!(map_stop_reason("unknown_value"), None);
    }

    #[test]
    fn decode_response_maps_finish_reason_via_map_stop_reason() {
        let wire = wire_response(
            vec![choice(assistant_message(Some("hi")), Some("length"))],
            None,
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(response.stop_reason, Some(StopReason::MaxTokens));
    }

    #[test]
    fn decode_response_with_no_finish_reason_has_none_stop_reason() {
        let wire = wire_response(vec![choice(assistant_message(Some("hi")), None)], None);
        let response = decode_response(wire).unwrap();
        assert_eq!(response.stop_reason, None);
    }

    // --- usage conversion (Requirement 4.6) ---

    #[test]
    fn decode_response_converts_usage_field() {
        let wire = wire_response(
            vec![choice(assistant_message(Some("hi")), Some("stop"))],
            Some(WireUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                ..Default::default()
            }),
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(
            response.usage,
            Some(Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
            })
        );
    }

    #[test]
    fn decode_usage_extracts_cache_read_tokens_from_prompt_tokens_details() {
        // zhipu / OpenAI 兼容形态
        let wire = wire_response(
            vec![choice(assistant_message(Some("hi")), Some("stop"))],
            Some(WireUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                prompt_tokens_details: Some(WireTokenDetails {
                    cached_tokens: Some(40),
                    reasoning_tokens: None,
                }),
                completion_tokens_details: Some(WireTokenDetails {
                    cached_tokens: None,
                    reasoning_tokens: Some(84),
                }),
                ..Default::default()
            }),
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(
            response.usage,
            Some(Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 40,
                reasoning_tokens: 84,
            })
        );
    }

    #[test]
    fn decode_usage_falls_back_to_prompt_tokens_details_reasoning_tokens() {
        // 部分 provider 把 reasoning_tokens 放在 prompt_tokens_details 下
        let wire = wire_response(
            vec![choice(assistant_message(Some("hi")), Some("stop"))],
            Some(WireUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                prompt_tokens_details: Some(WireTokenDetails {
                    cached_tokens: None,
                    reasoning_tokens: Some(33),
                }),
                completion_tokens_details: Some(WireTokenDetails {
                    cached_tokens: None,
                    reasoning_tokens: None,
                }),
                ..Default::default()
            }),
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(
            response.usage,
            Some(Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 0,
                reasoning_tokens: 33,
            })
        );
    }

    #[test]
    fn decode_usage_falls_back_to_kimi_top_level_cached_tokens() {
        // kimi：顶层 cached_tokens 与 prompt_tokens_details.cached_tokens 同时出现
        let wire = wire_response(
            vec![choice(assistant_message(Some("hi")), Some("stop"))],
            Some(WireUsage {
                prompt_tokens: 58,
                completion_tokens: 37,
                total_tokens: 95,
                cached_tokens: Some(58),
                prompt_tokens_details: Some(WireTokenDetails {
                    cached_tokens: Some(58),
                    reasoning_tokens: None,
                }),
                completion_tokens_details: Some(WireTokenDetails {
                    reasoning_tokens: Some(19),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(
            response.usage,
            Some(Usage {
                input_tokens: 58,
                output_tokens: 37,
                cache_read_tokens: 58,
                reasoning_tokens: 19,
            })
        );
    }

    #[test]
    fn decode_usage_falls_back_to_deepseek_prompt_cache_hit_tokens() {
        // deepseek：缺少 prompt_tokens_details.cached_tokens 时退化到
        // `prompt_cache_hit_tokens` 顶层冗余字段。
        let wire = wire_response(
            vec![choice(assistant_message(Some("hi")), Some("stop"))],
            Some(WireUsage {
                prompt_tokens: 308,
                completion_tokens: 62,
                total_tokens: 370,
                prompt_tokens_details: Some(WireTokenDetails {
                    cached_tokens: None,
                    reasoning_tokens: None,
                }),
                completion_tokens_details: Some(WireTokenDetails {
                    reasoning_tokens: Some(12),
                    ..Default::default()
                }),
                prompt_cache_hit_tokens: Some(256),
                prompt_cache_miss_tokens: Some(52),
                ..Default::default()
            }),
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(
            response.usage,
            Some(Usage {
                input_tokens: 308,
                output_tokens: 62,
                cache_read_tokens: 256,
                reasoning_tokens: 12,
            })
        );
    }

    #[test]
    fn decode_usage_with_no_cache_fields_has_zero_cache_read_tokens() {
        let wire = wire_response(
            vec![choice(assistant_message(Some("hi")), Some("stop"))],
            Some(WireUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                ..Default::default()
            }),
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(
            response.usage,
            Some(Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
            })
        );
    }

    #[test]
    fn decode_response_with_no_usage_has_none_usage() {
        let wire = wire_response(
            vec![choice(assistant_message(Some("hi")), Some("stop"))],
            None,
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(response.usage, None);
    }

    // --- successful decode paths ---

    #[test]
    fn decode_response_with_text_content_succeeds() {
        let wire = wire_response(
            vec![choice(assistant_message(Some("hello there")), Some("stop"))],
            None,
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(response.id, "chatcmpl-1");
        assert_eq!(response.model, "gpt-4");
        assert_eq!(response.role, Role::Assistant);
        assert_eq!(response.content.len(), 1);
        match &response.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello there"),
            other => panic!("expected Text block, got {other:?}"),
        }
        assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
    }

    #[test]
    fn decode_response_with_tool_calls_content_succeeds() {
        let message = WireResponseMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![WireResponseToolCall {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: WireResponseToolCallFunction {
                    name: "get_weather".to_string(),
                    arguments: "{\"city\":\"Beijing\"}".to_string(),
                },
            }]),
            ..Default::default()
        };
        let wire = wire_response(vec![choice(message, Some("tool_calls"))], None);
        let response = decode_response(wire).unwrap();
        assert_eq!(response.content.len(), 1);
        match &response.content[0] {
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "get_weather");
                assert_eq!(input, &serde_json::json!({"city": "Beijing"}));
            }
            other => panic!("expected ToolUse block, got {other:?}"),
        }
        assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
    }

    /// 非流式响应里的 `arguments` 原文同样要逐字节留下，供重放时回传。
    #[test]
    fn decode_response_keeps_raw_tool_arguments() {
        let raw = r#"{"city": "Beijing", "unit": "c"}"#;
        let message = WireResponseMessage {
            role: "assistant".to_string(),
            content: None,
            tool_calls: Some(vec![WireResponseToolCall {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: WireResponseToolCallFunction {
                    name: "get_weather".to_string(),
                    arguments: raw.to_string(),
                },
            }]),
            ..Default::default()
        };
        let wire = wire_response(vec![choice(message, Some("tool_calls"))], None);
        let response = decode_response(wire).unwrap();
        match &response.content[0] {
            ContentBlock::ToolUse { raw_arguments, .. } => {
                assert_eq!(raw_arguments.as_deref(), Some(raw));
            }
            other => panic!("expected ToolUse block, got {other:?}"),
        }
    }

    #[test]
    fn decode_response_with_text_and_tool_calls_produces_both_blocks() {
        let message = WireResponseMessage {
            role: "assistant".to_string(),
            content: Some("let me check".to_string()),
            tool_calls: Some(vec![WireResponseToolCall {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: WireResponseToolCallFunction {
                    name: "get_weather".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            ..Default::default()
        };
        let wire = wire_response(vec![choice(message, Some("tool_calls"))], None);
        let response = decode_response(wire).unwrap();
        assert_eq!(response.content.len(), 2);
        assert!(matches!(response.content[0], ContentBlock::Text { .. }));
        assert!(matches!(response.content[1], ContentBlock::ToolUse { .. }));
    }

    // -----------------------------------------------------------------------
    // StreamDecoder（任务 11，Requirement 5）
    // -----------------------------------------------------------------------

    use crate::domain::stream::StreamCollector;
    use crate::provider::completions::types::{WireStreamChoice, WireStreamToolCallFunction};

    fn stream_chunk(
        choices: Vec<WireStreamChoice>,
        usage: Option<WireUsage>,
    ) -> ChatCompletionChunk {
        ChatCompletionChunk {
            id: "chatcmpl-1".to_string(),
            model: "gpt-4".to_string(),
            choices,
            usage,
        }
    }

    fn text_delta_choice(text: &str, finish_reason: Option<&str>) -> WireStreamChoice {
        WireStreamChoice {
            index: 0,
            delta: WireStreamDelta {
                content: Some(text.to_string()),
                ..Default::default()
            },
            finish_reason: finish_reason.map(|s| s.to_string()),
        }
    }

    fn tool_delta_choice(
        wire_index: u32,
        id: Option<&str>,
        name: Option<&str>,
        arguments: Option<&str>,
        finish_reason: Option<&str>,
    ) -> WireStreamChoice {
        WireStreamChoice {
            index: 0,
            delta: WireStreamDelta {
                tool_calls: Some(vec![WireStreamToolCall {
                    index: wire_index,
                    id: id.map(|s| s.to_string()),
                    kind: id.map(|_| "function".to_string()),
                    function: Some(WireStreamToolCallFunction {
                        name: name.map(|s| s.to_string()),
                        arguments: arguments.map(|s| s.to_string()),
                    }),
                }]),
                ..Default::default()
            },
            finish_reason: finish_reason.map(|s| s.to_string()),
        }
    }

    fn empty_choice(finish_reason: Option<&str>) -> WireStreamChoice {
        WireStreamChoice {
            index: 0,
            delta: WireStreamDelta::default(),
            finish_reason: finish_reason.map(|s| s.to_string()),
        }
    }

    // --- MessageStart (Requirement 5.1) ---

    #[test]
    fn first_chunk_produces_message_start() {
        let mut decoder = StreamDecoder::new();
        let chunk = stream_chunk(vec![text_delta_choice("hi", None)], None);
        let events = decoder.decode_chunk(chunk).unwrap();
        match &events[0] {
            StreamEvent::MessageStart { id, model } => {
                assert_eq!(id, "chatcmpl-1");
                assert_eq!(model, "gpt-4");
            }
            other => panic!("expected MessageStart first, got {other:?}"),
        }
    }

    // --- text block lifecycle (Requirement 5.2) ---

    #[test]
    fn text_delta_opens_block_once_then_emits_deltas() {
        let mut decoder = StreamDecoder::new();
        let events1 = decoder
            .decode_chunk(stream_chunk(vec![text_delta_choice("Hel", None)], None))
            .unwrap();
        // MessageStart + ContentBlockStart + ContentBlockDelta
        assert_eq!(events1.len(), 3);
        assert!(matches!(
            events1[1],
            StreamEvent::ContentBlockStart {
                index: 0,
                block: ContentBlock::Text { .. }
            }
        ));
        assert!(matches!(
            &events1[2],
            StreamEvent::ContentBlockDelta { index: 0, delta: Delta::TextDelta { text } } if text == "Hel"
        ));

        let events2 = decoder
            .decode_chunk(stream_chunk(vec![text_delta_choice("lo", None)], None))
            .unwrap();
        // no MessageStart, no ContentBlockStart, only a delta this time.
        assert_eq!(events2.len(), 1);
        assert!(matches!(
            &events2[0],
            StreamEvent::ContentBlockDelta { index: 0, delta: Delta::TextDelta { text } } if text == "lo"
        ));
    }

    // --- tool call block lifecycle (Requirement 5.3) ---

    #[test]
    fn tool_call_delta_opens_block_per_distinct_index() {
        let mut decoder = StreamDecoder::new();
        let events1 = decoder
            .decode_chunk(stream_chunk(
                vec![tool_delta_choice(
                    0,
                    Some("call_1"),
                    Some("get_weather"),
                    Some("{\"c"),
                    None,
                )],
                None,
            ))
            .unwrap();
        // MessageStart + ContentBlockStart + ContentBlockDelta
        assert_eq!(events1.len(), 3);
        match &events1[1] {
            StreamEvent::ContentBlockStart { index, block } => {
                assert_eq!(*index, 0);
                match block {
                    ContentBlock::ToolUse { id, name, .. } => {
                        assert_eq!(id, "call_1");
                        assert_eq!(name, "get_weather");
                    }
                    other => panic!("expected ToolUse block, got {other:?}"),
                }
            }
            other => panic!("expected ContentBlockStart, got {other:?}"),
        }

        // Same wire index -> should reuse block 0, only emit a delta.
        let events2 = decoder
            .decode_chunk(stream_chunk(
                vec![tool_delta_choice(0, None, None, Some("ity\":1}"), None)],
                None,
            ))
            .unwrap();
        assert_eq!(events2.len(), 1);
        assert!(matches!(
            &events2[0],
            StreamEvent::ContentBlockDelta { index: 0, .. }
        ));

        // Different wire index -> a new block (index 1).
        let events3 = decoder
            .decode_chunk(stream_chunk(
                vec![tool_delta_choice(
                    1,
                    Some("call_2"),
                    Some("other_tool"),
                    None,
                    None,
                )],
                None,
            ))
            .unwrap();
        assert_eq!(events3.len(), 1);
        assert!(matches!(
            &events3[0],
            StreamEvent::ContentBlockStart { index: 1, .. }
        ));
    }

    // --- finish_reason closes open blocks (Requirement 5.4) ---

    #[test]
    fn finish_reason_closes_open_blocks_then_emits_message_delta() {
        let mut decoder = StreamDecoder::new();
        decoder
            .decode_chunk(stream_chunk(vec![text_delta_choice("hi", None)], None))
            .unwrap();
        decoder
            .decode_chunk(stream_chunk(
                vec![tool_delta_choice(
                    0,
                    Some("call_1"),
                    Some("get_weather"),
                    Some("{}"),
                    None,
                )],
                None,
            ))
            .unwrap();

        let events = decoder
            .decode_chunk(stream_chunk(vec![empty_choice(Some("stop"))], None))
            .unwrap();

        // Both open blocks (text=0, tool=1) should be closed, in ascending order,
        // followed by exactly one MessageDelta.
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events[0],
            StreamEvent::ContentBlockStop { index: 0 }
        ));
        assert!(matches!(
            events[1],
            StreamEvent::ContentBlockStop { index: 1 }
        ));
        match &events[2] {
            StreamEvent::MessageDelta { stop_reason, .. } => {
                assert_eq!(*stop_reason, Some(StopReason::EndTurn));
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
    }

    // --- AwaitingDone behavior (Requirement 5.5, 5.7) ---

    #[test]
    fn awaiting_done_with_nonempty_choices_returns_data_after_finish() {
        let mut decoder = StreamDecoder::new();
        decoder
            .decode_chunk(stream_chunk(
                vec![text_delta_choice("hi", Some("stop"))],
                None,
            ))
            .unwrap();

        let err = decoder
            .decode_chunk(stream_chunk(vec![text_delta_choice("more", None)], None))
            .unwrap_err();
        assert!(matches!(err, CompletionsDecodeError::DataAfterFinish));
    }

    #[test]
    fn awaiting_done_with_empty_choices_and_no_usage_is_noop() {
        let mut decoder = StreamDecoder::new();
        decoder
            .decode_chunk(stream_chunk(
                vec![text_delta_choice("hi", Some("stop"))],
                None,
            ))
            .unwrap();

        let events = decoder.decode_chunk(stream_chunk(vec![], None)).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn trailing_usage_chunk_reaches_response_usage() {
        let mut decoder = StreamDecoder::new();
        let mut collector = StreamCollector::new();
        for chunk in [
            stream_chunk(vec![text_delta_choice("hi", None)], None),
            stream_chunk(vec![empty_choice(Some("stop"))], None),
            stream_chunk(
                vec![],
                Some(WireUsage {
                    prompt_tokens: 7,
                    completion_tokens: 11,
                    total_tokens: 18,
                    ..Default::default()
                }),
            ),
        ] {
            for event in decoder.decode_chunk(chunk).unwrap() {
                collector.push(&event);
            }
        }
        for event in decoder.finish().unwrap() {
            collector.push(&event);
        }

        let response = collector.finish().unwrap();
        assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(
            response.usage,
            Some(Usage {
                input_tokens: 7,
                output_tokens: 11,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
            })
        );
        assert!(matches!(
            &response.content[0],
            ContentBlock::Text { text } if text == "hi"
        ));
    }

    // --- finish() (Requirement 5.6) ---

    #[test]
    fn finish_from_streaming_closes_blocks_and_emits_message_delta_and_stop() {
        let mut decoder = StreamDecoder::new();
        decoder
            .decode_chunk(stream_chunk(vec![text_delta_choice("hi", None)], None))
            .unwrap();

        let events = decoder.finish().unwrap();
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events[0],
            StreamEvent::ContentBlockStop { index: 0 }
        ));
        match &events[1] {
            StreamEvent::MessageDelta { stop_reason, usage } => {
                assert_eq!(*stop_reason, None);
                assert_eq!(*usage, None);
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
        assert!(matches!(events[2], StreamEvent::MessageStop));
    }

    #[test]
    fn finish_from_awaiting_done_emits_only_message_stop() {
        let mut decoder = StreamDecoder::new();
        decoder
            .decode_chunk(stream_chunk(
                vec![text_delta_choice("hi", Some("stop"))],
                None,
            ))
            .unwrap();

        let events = decoder.finish().unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::MessageStop));
    }

    #[test]
    fn finish_from_initial_returns_missing_choice() {
        let mut decoder = StreamDecoder::new();
        let err = decoder.finish().unwrap_err();
        assert!(matches!(err, CompletionsDecodeError::MissingChoice));
    }

    // --- Stopped rejects everything (Requirement 5.7) ---

    #[test]
    fn stopped_decode_chunk_and_finish_return_data_after_stop() {
        let mut decoder = StreamDecoder::new();
        decoder
            .decode_chunk(stream_chunk(
                vec![text_delta_choice("hi", Some("stop"))],
                None,
            ))
            .unwrap();
        decoder.finish().unwrap();

        let err1 = decoder
            .decode_chunk(stream_chunk(vec![text_delta_choice("more", None)], None))
            .unwrap_err();
        assert!(matches!(err1, CompletionsDecodeError::DataAfterStop));

        let err2 = decoder.finish().unwrap_err();
        assert!(matches!(err2, CompletionsDecodeError::DataAfterStop));
    }
}

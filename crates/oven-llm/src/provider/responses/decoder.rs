//! decoder：将 OpenAI Responses API 的 wire 格式响应转换为 domain 层的
//! `Response` / `StreamEvent`。
//!
//! 非流式部分：`decode_response`。
//! 流式部分：`StreamDecoder`，把 Responses API 的按 `output_index` 组织的
//! 事件流升维为 Anthropic 风格的块事件序列。

use std::collections::{HashMap, HashSet};

use thiserror::Error;

use super::types::{
    ResponseEvent, ResponseObject, ResponseOutputItem, WireOutputContentPart, WireUsage,
};
use crate::domain::message::{ContentBlock, Role};
use crate::domain::response::{Response, StopReason, Usage};
use crate::domain::stream::{Delta, StreamEvent, StreamPhase};

/// `decode_response`（及流式 `StreamDecoder`）的解码失败原因。
#[derive(Debug, Error)]
pub enum DecodeError {
    /// wire 响应的 `status == "failed"`（`response.error` 已填）。
    #[error("response failed: {message}")]
    Failed { message: String },
    /// 某个 function_call 的 `arguments` 字段不是合法 JSON。
    #[error("invalid tool arguments JSON for tool call {id}: {source}")]
    InvalidToolArguments {
        id: String,
        #[source]
        source: serde_json::Error,
    },
    /// 通用 JSON 反序列化错误（例如 SSE 事件解析失败）。
    #[error("JSON deserialization error: {0}")]
    Json(#[source] serde_json::Error),
    /// 流式状态机在收到终止事件（`response.completed` /
    /// `response.incomplete`）之后又收到了内容事件。
    #[error("received data after finish")]
    DataAfterFinish,
    /// 流式状态机在 `Stopped` 之后又被调用。
    #[error("received data after stream stopped")]
    DataAfterStop,
    /// 流式状态机从未收到 `response.created` 就结束。
    #[error("stream ended before response.created")]
    MissingStart,
}

/// 将 `WireUsage` 转换为 domain 层的 `Usage`：
/// `input_tokens_details.cached_tokens` → `cache_read_tokens`，
/// `output_tokens_details.reasoning_tokens` → `reasoning_tokens`。
pub(crate) fn decode_usage(usage: WireUsage) -> Usage {
    Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage
            .input_tokens_details
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0),
        reasoning_tokens: usage
            .output_tokens_details
            .and_then(|d| d.reasoning_tokens)
            .unwrap_or(0),
    }
}

/// 计算 domain 层的 `StopReason`：
///
/// - 输出中出现任意 function_call → `ToolUse`；
/// - 否则 `status == "completed"` → `EndTurn`；
/// - `status == "incomplete"` 且 `incomplete_details.reason ==
///   "max_output_tokens"` → `MaxTokens`；
/// - 其余（`content_filter` 等）→ `None`。
fn map_stop_reason(
    status: Option<&str>,
    incomplete_reason: Option<&str>,
    saw_function_call: bool,
) -> Option<StopReason> {
    if saw_function_call {
        return Some(StopReason::ToolUse);
    }
    match status {
        Some("completed") => Some(StopReason::EndTurn),
        Some("incomplete") if incomplete_reason == Some("max_output_tokens") => {
            Some(StopReason::MaxTokens)
        }
        _ => None,
    }
}

/// 将 OpenAI Responses API 的非流式响应体解码为 domain 层的 `Response`。
///
/// - `status == "failed"` → `DecodeError::Failed`
/// - `output` 按顺序转换：message → `Text`、reasoning → `Thinking`、
///   function_call → `ToolUse`（`arguments` 解析失败 →
///   `DecodeError::InvalidToolArguments`）、web_search_call / 未知类型跳过
/// - `usage`（若存在）转换为 `Usage`
/// - `stop_reason` 按 `map_stop_reason` 计算
pub(crate) fn decode_response(wire: ResponseObject) -> Result<Response, DecodeError> {
    if wire.status.as_deref() == Some("failed") {
        let message = wire
            .error
            .as_ref()
            .and_then(|error| error.message.clone())
            .unwrap_or_default();
        return Err(DecodeError::Failed { message });
    }

    let mut content = Vec::new();
    let mut saw_function_call = false;

    for item in wire.output {
        match item {
            ResponseOutputItem::Message { content: parts, .. } => {
                for part in parts {
                    if let WireOutputContentPart::OutputText { text } = part
                        && !text.is_empty()
                    {
                        content.push(ContentBlock::Text { text });
                    }
                }
            }
            ResponseOutputItem::Reasoning { summary, .. } => {
                let thinking: String = summary.iter().map(|summary| summary.text.clone()).collect();
                if !thinking.is_empty() {
                    content.push(ContentBlock::Thinking { thinking });
                }
            }
            ResponseOutputItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                let input: serde_json::Value =
                    serde_json::from_str(&arguments).map_err(|source| {
                        DecodeError::InvalidToolArguments {
                            id: call_id.clone(),
                            source,
                        }
                    })?;
                content.push(ContentBlock::ToolUse {
                    id: call_id,
                    name,
                    input,
                    // 原样保留 wire 上的 `arguments` 文本，供重放时逐字节回传
                    // （provider 隐式上下文缓存要求完整匹配缓存单元）。
                    raw_arguments: Some(arguments),
                });
                saw_function_call = true;
            }
            ResponseOutputItem::WebSearchCall { .. } | ResponseOutputItem::Other(_) => {}
        }
    }

    let stop_reason = map_stop_reason(
        wire.status.as_deref(),
        wire.incomplete_details
            .as_ref()
            .and_then(|details| details.reason.as_deref()),
        saw_function_call,
    );

    Ok(Response {
        id: wire.id,
        model: wire.model,
        role: Role::Assistant,
        content,
        stop_reason,
        usage: wire.usage.map(decode_usage),
    })
}

// ---------------------------------------------------------------------------
// 流式 StreamDecoder
// ---------------------------------------------------------------------------

/// 将 OpenAI Responses API 流式事件升维为 Anthropic 风格的块事件序列
/// （`ContentBlockStart → ContentBlockDelta* → ContentBlockStop`）。
///
/// 状态机转换：`Initial → Streaming → AwaitingDone → Stopped`。终止事件由
/// `response.completed` / `response.incomplete` 触发；`response.failed` 直接
/// 返回 `DecodeError::Failed` 并终止。
#[derive(Debug, Default)]
pub(crate) struct StreamDecoder {
    phase: StreamPhase,
    /// wire 的 `output_index` → 本解码器分配的内容块索引。一个输出项只对应
    /// 一个内容块（`content_index` 被忽略）。
    item_block_map: HashMap<u64, usize>,
    /// 当前仍处于「已开启但未关闭」状态的内容块索引集合。
    open_blocks: HashSet<usize>,
    /// 下一个待分配的内容块索引。
    next_block_index: usize,
    /// 是否出现过 function_call 输出项（决定终止时的 `stop_reason`）。
    saw_tool_call: bool,
}

impl StreamDecoder {
    /// 构造一个处于 `Initial` 阶段的新解码器。
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 当前是否已收到终止事件（等待 `finish()` 产出 `MessageStop`）。
    pub(crate) fn is_awaiting_done(&self) -> bool {
        self.phase == StreamPhase::AwaitingDone
    }

    /// 处理一个 SSE 事件，返回本次调用产出的事件列表（可能为空）。
    ///
    /// - `Stopped` 阶段调用 → `DecodeError::DataAfterStop`
    /// - `AwaitingDone` 阶段收到内容事件 → `DecodeError::DataAfterFinish`
    /// - `response.created`（仅 `Initial`）→ 产出 `MessageStart`
    /// - `response.completed` / `response.incomplete` → 关闭剩余块 +
    ///   `MessageDelta`，转入 `AwaitingDone`
    /// - `response.failed` → `DecodeError::Failed`
    pub(crate) fn decode_event(
        &mut self,
        event: ResponseEvent,
    ) -> Result<Vec<StreamEvent>, DecodeError> {
        if self.phase == StreamPhase::Stopped {
            return Err(DecodeError::DataAfterStop);
        }
        if self.phase == StreamPhase::AwaitingDone {
            return Err(DecodeError::DataAfterFinish);
        }

        let mut events = Vec::new();

        match event {
            ResponseEvent::ResponseCreated { response } => {
                if self.phase == StreamPhase::Initial {
                    events.push(StreamEvent::MessageStart {
                        id: response.id,
                        model: response.model,
                    });
                    self.phase = StreamPhase::Streaming;
                }
            }
            ResponseEvent::ResponseInProgress { .. } => {}
            ResponseEvent::ResponseOutputItemAdded { output_index, item } => {
                self.dispatch_item_added(output_index, item, &mut events);
            }
            ResponseEvent::ResponseOutputItemDone { output_index, .. } => {
                if let Some(index) = self.item_block_map.remove(&output_index) {
                    self.open_blocks.remove(&index);
                    events.push(StreamEvent::ContentBlockStop { index });
                }
            }
            ResponseEvent::ResponseContentPartAdded { .. }
            | ResponseEvent::ResponseContentPartDone { .. }
            | ResponseEvent::ResponseOutputTextDone { .. }
            | ResponseEvent::ResponseReasoningTextDone { .. }
            | ResponseEvent::ResponseReasoningSummaryTextDone { .. }
            | ResponseEvent::ResponseFunctionCallArgumentsDone { .. } => {}
            ResponseEvent::ResponseOutputTextDelta {
                output_index,
                delta,
            } => {
                let index = self.ensure_text_block(output_index, &mut events);
                if !delta.is_empty() {
                    events.push(StreamEvent::ContentBlockDelta {
                        index,
                        delta: Delta::TextDelta { text: delta },
                    });
                }
            }
            ResponseEvent::ResponseReasoningTextDelta {
                output_index,
                delta,
            }
            | ResponseEvent::ResponseReasoningSummaryTextDelta {
                output_index,
                delta,
            } => {
                let index = self.ensure_thinking_block(output_index, &mut events);
                if !delta.is_empty() {
                    events.push(StreamEvent::ContentBlockDelta {
                        index,
                        delta: Delta::ThinkingDelta { thinking: delta },
                    });
                }
            }
            ResponseEvent::ResponseFunctionCallArgumentsDelta {
                output_index,
                delta,
            } => {
                let index = self.ensure_tool_block(output_index, None, None, &mut events);
                self.saw_tool_call = true;
                if !delta.is_empty() {
                    events.push(StreamEvent::ContentBlockDelta {
                        index,
                        delta: Delta::InputJsonDelta {
                            partial_json: delta,
                        },
                    });
                }
            }
            ResponseEvent::ResponseCompleted { response } => {
                self.close_open_blocks(&mut events);
                events.push(StreamEvent::MessageDelta {
                    stop_reason: map_stop_reason(Some("completed"), None, self.saw_tool_call),
                    usage: response.usage.map(decode_usage),
                });
                self.phase = StreamPhase::AwaitingDone;
            }
            ResponseEvent::ResponseIncomplete { response } => {
                self.close_open_blocks(&mut events);
                let reason = response
                    .incomplete_details
                    .as_ref()
                    .and_then(|details| details.reason.as_deref());
                events.push(StreamEvent::MessageDelta {
                    stop_reason: map_stop_reason(Some("incomplete"), reason, self.saw_tool_call),
                    usage: response.usage.map(decode_usage),
                });
                self.phase = StreamPhase::AwaitingDone;
            }
            ResponseEvent::ResponseFailed { response } => {
                let message = response
                    .error
                    .as_ref()
                    .and_then(|error| error.message.clone())
                    .unwrap_or_default();
                self.phase = StreamPhase::Stopped;
                return Err(DecodeError::Failed { message });
            }
            ResponseEvent::Other(_) => {}
        }

        Ok(events)
    }

    /// 结束流式解码，产出 `MessageStop` 并转入 `Stopped`。
    ///
    /// - `Initial` 阶段（从未收到 `response.created`）→
    ///   `DecodeError::MissingStart`
    /// - `Stopped` 阶段 → `DecodeError::DataAfterStop`
    /// - `AwaitingDone` 阶段：`MessageDelta` 已发出，此处只需产出
    ///   `MessageStop`
    /// - `Streaming` 阶段（流结束前从未收到终止事件）：补齐未关闭块的
    ///   `ContentBlockStop` 与一个 `stop_reason`/`usage` 均为 `None` 的
    ///   `MessageDelta`，再产出 `MessageStop`
    pub(crate) fn finish(&mut self) -> Result<Vec<StreamEvent>, DecodeError> {
        match self.phase {
            StreamPhase::Initial => Err(DecodeError::MissingStart),
            StreamPhase::Stopped => Err(DecodeError::DataAfterStop),
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

    /// 分派 `response.output_item.added`：按输出项类型开启对应内容块。
    ///
    /// message → `Text`、reasoning → `Thinking`、function_call → `ToolUse`
    /// （id 取 `call_id`，input 用空字符串占位，由上层 `StreamCollector`
    /// 拼接 `InputJsonDelta` 后解析）；web_search_call / 未知类型跳过。
    fn dispatch_item_added(
        &mut self,
        output_index: u64,
        item: ResponseOutputItem,
        events: &mut Vec<StreamEvent>,
    ) {
        match item {
            ResponseOutputItem::Message { .. } => {
                self.ensure_text_block(output_index, events);
            }
            ResponseOutputItem::Reasoning { .. } => {
                self.ensure_thinking_block(output_index, events);
            }
            ResponseOutputItem::FunctionCall { call_id, name, .. } => {
                self.ensure_tool_block(output_index, Some(&call_id), Some(&name), events);
                self.saw_tool_call = true;
            }
            ResponseOutputItem::WebSearchCall { .. } | ResponseOutputItem::Other(_) => {}
        }
    }

    /// 查找或分配 `output_index` 对应的文本块；首次分配时产出
    /// `ContentBlockStart` 事件。
    fn ensure_text_block(&mut self, output_index: u64, events: &mut Vec<StreamEvent>) -> usize {
        self.ensure_block(
            output_index,
            ContentBlock::Text {
                text: String::new(),
            },
            events,
        )
    }

    /// 查找或分配 `output_index` 对应的思维块；首次分配时产出
    /// `ContentBlockStart` 事件。
    fn ensure_thinking_block(&mut self, output_index: u64, events: &mut Vec<StreamEvent>) -> usize {
        self.ensure_block(
            output_index,
            ContentBlock::Thinking {
                thinking: String::new(),
            },
            events,
        )
    }

    /// 查找或分配 `output_index` 对应的工具调用块；首次分配时产出
    /// `ContentBlockStart` 事件。
    ///
    /// `input` 使用空字符串 `serde_json::Value::String(String::new())` 作为
    /// 占位：流式阶段 `arguments` 只是逐步到达的原始字符串片段，尚不构成
    /// 合法 JSON，无法解析为最终的参数对象；真正的参数值由上层在收集完所有
    /// `InputJsonDelta` 片段后自行拼接解析。
    fn ensure_tool_block(
        &mut self,
        output_index: u64,
        id: Option<&str>,
        name: Option<&str>,
        events: &mut Vec<StreamEvent>,
    ) -> usize {
        let block = ContentBlock::ToolUse {
            id: id.unwrap_or_default().to_string(),
            name: name.unwrap_or_default().to_string(),
            input: serde_json::Value::String(String::new()),
            raw_arguments: None,
        };
        self.ensure_block(output_index, block, events)
    }

    /// 查找或分配 `output_index` 对应的内容块索引；首次分配时产出
    /// `ContentBlockStart` 事件。
    fn ensure_block(
        &mut self,
        output_index: u64,
        block: ContentBlock,
        events: &mut Vec<StreamEvent>,
    ) -> usize {
        if let Some(&index) = self.item_block_map.get(&output_index) {
            return index;
        }

        let index = self.allocate_block_index();
        self.item_block_map.insert(output_index, index);
        self.open_blocks.insert(index);
        events.push(StreamEvent::ContentBlockStart { index, block });
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
    use super::super::testdata::{deepseek_sse, grok_sse};
    use super::*;
    use crate::domain::stream::StreamCollector;

    fn wire_response(
        status: &str,
        output: serde_json::Value,
        usage: Option<WireUsage>,
        incomplete_reason: Option<&str>,
    ) -> ResponseObject {
        ResponseObject {
            id: "resp_1".to_string(),
            model: "deepseek-v4-flash".to_string(),
            status: Some(status.to_string()),
            error: None,
            incomplete_details: incomplete_reason.map(|reason| {
                crate::provider::responses::types::WireIncompleteDetails {
                    reason: Some(reason.to_string()),
                }
            }),
            output: serde_json::from_value(output).unwrap(),
            usage,
        }
    }

    fn sample_usage() -> WireUsage {
        WireUsage {
            input_tokens: 268,
            output_tokens: 41,
            input_tokens_details: Some(crate::provider::responses::types::WireInputTokensDetails {
                cached_tokens: Some(256),
            }),
            output_tokens_details: Some(
                crate::provider::responses::types::WireOutputTokensDetails {
                    reasoning_tokens: Some(0),
                },
            ),
        }
    }

    /// 解析内联的 SSE 文本为事件序列（测试用）。
    ///
    /// `str::lines()` 按 `\n` 切分并剥离行尾 `\r`，因此同时兼容 LF 与
    /// Windows CRLF，无需手动替换换行符；空行表示事件边界。每个事件块取
    /// 第一条 `data: ` 行的载荷。
    fn log_events_from_sse(sse: &str) -> Vec<ResponseEvent> {
        let mut blocks = vec![String::new()];
        for line in sse.lines() {
            if line.is_empty() {
                blocks.push(String::new());
            } else if let Some(block) = blocks.last_mut() {
                block.push_str(line);
                block.push('\n');
            }
        }

        blocks
            .iter()
            .filter_map(|block| {
                let data = block.lines().find(|line| line.starts_with("data: "))?;
                let json: serde_json::Value =
                    serde_json::from_str(data.trim_start_matches("data: ")).ok()?;
                serde_json::from_value(json).ok()
            })
            .collect()
    }

    // --- 非流式 decode_response ---

    #[test]
    fn decode_response_with_reasoning_message_and_usage() {
        let wire = wire_response(
            "completed",
            serde_json::json!([
                {
                    "type": "reasoning",
                    "id": "rs_1",
                    "summary": [
                        {"type": "summary_text", "text": "let me think"},
                        {"type": "summary_text", "text": " carefully"}
                    ]
                },
                {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "the answer"}]
                }
            ]),
            Some(sample_usage()),
            None,
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(response.id, "resp_1");
        assert_eq!(response.model, "deepseek-v4-flash");
        assert_eq!(response.role, Role::Assistant);
        assert_eq!(response.content.len(), 2);
        assert!(matches!(
            &response.content[0],
            ContentBlock::Thinking { thinking } if thinking == "let me think carefully"
        ));
        assert!(matches!(
            &response.content[1],
            ContentBlock::Text { text } if text == "the answer"
        ));
        assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(
            response.usage,
            Some(Usage {
                input_tokens: 268,
                output_tokens: 41,
                cache_read_tokens: 256,
                reasoning_tokens: 0,
            })
        );
    }

    #[test]
    fn decode_response_function_call_parses_arguments() {
        let wire = wire_response(
            "completed",
            serde_json::json!([{
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "{\"city\":\"Hangzhou\"}"
            }]),
            None,
            None,
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(response.content.len(), 1);
        match &response.content[0] {
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "get_weather");
                assert_eq!(input, &serde_json::json!({"city": "Hangzhou"}));
            }
            other => panic!("expected ToolUse block, got {other:?}"),
        }
        assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
    }

    /// 非流式 `function_call` 的 `arguments` 原文同样要逐字节留下。
    #[test]
    fn decode_response_keeps_raw_function_call_arguments() {
        let raw = r#"{ "city": "Hangzhou" }"#;
        let wire = wire_response(
            "completed",
            serde_json::json!([{
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": raw
            }]),
            None,
            None,
        );
        let response = decode_response(wire).unwrap();
        match &response.content[0] {
            ContentBlock::ToolUse { raw_arguments, .. } => {
                assert_eq!(raw_arguments.as_deref(), Some(raw));
            }
            other => panic!("expected ToolUse block, got {other:?}"),
        }
    }

    #[test]
    fn decode_response_invalid_arguments_errors() {
        let wire = wire_response(
            "completed",
            serde_json::json!([{
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "get_weather",
                "arguments": "not json"
            }]),
            None,
            None,
        );
        let err = decode_response(wire).unwrap_err();
        match err {
            DecodeError::InvalidToolArguments { id, .. } => assert_eq!(id, "call_1"),
            other => panic!("expected InvalidToolArguments, got {other:?}"),
        }
    }

    #[test]
    fn decode_response_incomplete_max_output_tokens_maps_to_max_tokens() {
        let wire = wire_response(
            "incomplete",
            serde_json::json!([
                {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "partial"}]
                }
            ]),
            None,
            Some("max_output_tokens"),
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(response.stop_reason, Some(StopReason::MaxTokens));
    }

    #[test]
    fn decode_response_incomplete_content_filter_maps_to_none() {
        let wire = wire_response(
            "incomplete",
            serde_json::json!([]),
            None,
            Some("content_filter"),
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(response.stop_reason, None);
    }

    #[test]
    fn decode_response_failed_errors() {
        let mut wire = wire_response("failed", serde_json::json!([]), None, None);
        wire.error = Some(crate::provider::responses::types::WireError {
            code: Some("server_error".to_string()),
            message: Some("boom".to_string()),
            param: None,
        });
        let err = decode_response(wire).unwrap_err();
        match err {
            DecodeError::Failed { message } => assert_eq!(message, "boom"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn decode_response_skips_web_search_call_and_unknown_items() {
        let wire = wire_response(
            "completed",
            serde_json::json!([
                {"type": "web_search_call", "id": "ws_1"},
                {"type": "message", "id": "msg_1", "role": "assistant",
                 "content": [{"type": "output_text", "text": "hi"}]},
                {"type": "future_item_type", "id": "x"}
            ]),
            None,
            None,
        );
        let response = decode_response(wire).unwrap();
        assert_eq!(response.content.len(), 1);
        assert!(matches!(&response.content[0], ContentBlock::Text { text } if text == "hi"));
    }

    // --- 流式：deepseek 日志重放 ---

    #[test]
    fn deepseek_log_replay_produces_expected_event_sequence() {
        let events = log_events_from_sse(&deepseek_sse());
        assert!(!events.is_empty());
        assert!(matches!(events[0], ResponseEvent::ResponseCreated { .. }));

        let mut decoder = StreamDecoder::new();
        let mut decoded = Vec::new();
        for event in events {
            decoded.extend(decoder.decode_event(event).unwrap());
        }
        let mut tail = decoder.finish().unwrap();
        decoded.append(&mut tail);

        // MessageStart from response.created
        match &decoded[0] {
            StreamEvent::MessageStart { id, model } => {
                assert_eq!(id, "be91d05d-ff00-4efb-b63a-a1a96d13c7a8");
                assert_eq!(model, "deepseek-v4-flash");
            }
            other => panic!("expected MessageStart first, got {other:?}"),
        }

        // 文本块：ContentBlockStart(0, Text) → 14 × TextDelta → ContentBlockStop(0)
        assert!(matches!(
            decoded[1],
            StreamEvent::ContentBlockStart {
                index: 0,
                block: ContentBlock::Text { .. }
            }
        ));
        let text_delta_count = decoded
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    StreamEvent::ContentBlockDelta {
                        index: 0,
                        delta: Delta::TextDelta { .. }
                    }
                )
            })
            .count();
        assert_eq!(text_delta_count, 14);
        assert!(matches!(
            decoded[2],
            StreamEvent::ContentBlockDelta {
                index: 0,
                delta: Delta::TextDelta { .. }
            }
        ));

        // 工具块：ContentBlockStart(1, ToolUse{call_id, name}) →
        // InputJsonDelta("{}") → ContentBlockStop(1)
        match &decoded[1 + 1 + 14 + 1] {
            StreamEvent::ContentBlockStart { index, block } => {
                assert_eq!(*index, 1);
                match block {
                    ContentBlock::ToolUse { id, name, .. } => {
                        assert_eq!(id, "call_00_C3qv97zqcLotY0gqaJxB7908");
                        assert_eq!(name, "get_weather");
                    }
                    other => panic!("expected ToolUse block, got {other:?}"),
                }
            }
            other => panic!("expected tool ContentBlockStart, got {other:?}"),
        }
        assert!(decoded.iter().any(|event| {
            matches!(
                event,
                StreamEvent::ContentBlockDelta {
                    index: 1,
                    delta: Delta::InputJsonDelta { partial_json }
                } if partial_json == "{}"
            )
        }));

        // 终止：MessageDelta{stop_reason: ToolUse, usage} + MessageStop
        match decoded.last().unwrap() {
            StreamEvent::MessageStop => {}
            other => panic!("expected MessageStop last, got {other:?}"),
        }
        match &decoded[decoded.len() - 2] {
            StreamEvent::MessageDelta { stop_reason, usage } => {
                assert_eq!(*stop_reason, Some(StopReason::ToolUse));
                let usage = usage.unwrap();
                assert_eq!(usage.input_tokens, 268);
                assert_eq!(usage.output_tokens, 41);
                assert_eq!(usage.cache_read_tokens, 256);
                assert_eq!(usage.reasoning_tokens, 0);
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
    }

    #[test]
    fn deepseek_log_replay_collects_into_response() {
        let events = log_events_from_sse(&deepseek_sse());
        let mut decoder = StreamDecoder::new();
        let mut collector = StreamCollector::new();
        for event in events {
            for stream_event in decoder.decode_event(event).unwrap() {
                collector.push(&stream_event);
            }
        }
        for stream_event in decoder.finish().unwrap() {
            collector.push(&stream_event);
        }

        let response = collector.finish().unwrap();
        assert_eq!(response.id, "be91d05d-ff00-4efb-b63a-a1a96d13c7a8");
        assert_eq!(response.model, "deepseek-v4-flash");
        assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(
            response.usage,
            Some(Usage {
                input_tokens: 268,
                output_tokens: 41,
                cache_read_tokens: 256,
                reasoning_tokens: 0,
            })
        );
        assert_eq!(response.content.len(), 2);
        assert!(matches!(
            &response.content[0],
            ContentBlock::Text { text } if text == "I'll check the weather in Hangzhou, Zhejiang for you."
        ));
        match &response.content[1] {
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                assert_eq!(id, "call_00_C3qv97zqcLotY0gqaJxB7908");
                assert_eq!(name, "get_weather");
                assert_eq!(input, &serde_json::json!({}));
            }
            other => panic!("expected ToolUse block, got {other:?}"),
        }
    }

    // --- 流式：grok 日志重放 ---

    #[test]
    fn grok_log_replay_produces_thinking_block() {
        let events = log_events_from_sse(&grok_sse());
        assert!(!events.is_empty());

        let mut decoder = StreamDecoder::new();
        let mut collector = StreamCollector::new();
        for event in events {
            for stream_event in decoder.decode_event(event).unwrap() {
                collector.push(&stream_event);
            }
        }
        for stream_event in decoder.finish().unwrap() {
            collector.push(&stream_event);
        }

        let response = collector.finish().unwrap();
        assert_eq!(response.model, "grok-build-0.1");
        assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(
            response.usage,
            Some(Usage {
                input_tokens: 304,
                output_tokens: 149,
                cache_read_tokens: 256,
                reasoning_tokens: 137,
            })
        );
        assert_eq!(response.content.len(), 2);
        assert!(matches!(
            &response.content[0],
            ContentBlock::Thinking { thinking } if thinking == "The question is: \"What is the temperature in San Francisco?\"\n"
        ));
        match &response.content[1] {
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                assert_eq!(id, "call-b9f824e6-aba8-4c03-865d-e9c485976c0f-0");
                assert_eq!(name, "get_temperature");
                assert_eq!(input, &serde_json::json!({"location": "San Francisco"}));
            }
            other => panic!("expected ToolUse block, got {other:?}"),
        }
    }

    // --- 流式：终止与状态机 ---

    #[test]
    fn streaming_incomplete_terminates_with_max_tokens() {
        let mut decoder = StreamDecoder::new();
        decoder
            .decode_event(
                serde_json::from_value(serde_json::json!({
                    "type": "response.created",
                    "response": {"id": "r1", "model": "m1", "output": []}
                }))
                .unwrap(),
            )
            .unwrap();
        let events = decoder
            .decode_event(
                serde_json::from_value(serde_json::json!({
                    "type": "response.incomplete",
                    "response": {
                        "id": "r1",
                        "model": "m1",
                        "status": "incomplete",
                        "incomplete_details": {"reason": "max_output_tokens"},
                        "output": []
                    }
                }))
                .unwrap(),
            )
            .unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::MessageDelta { stop_reason, .. } => {
                assert_eq!(*stop_reason, Some(StopReason::MaxTokens));
            }
            other => panic!("expected MessageDelta, got {other:?}"),
        }
        assert!(decoder.is_awaiting_done());
        assert!(matches!(
            decoder.finish().unwrap()[0],
            StreamEvent::MessageStop
        ));
    }

    #[test]
    fn streaming_failed_returns_failed_error() {
        let mut decoder = StreamDecoder::new();
        decoder
            .decode_event(
                serde_json::from_value(serde_json::json!({
                    "type": "response.created",
                    "response": {"id": "r1", "model": "m1", "output": []}
                }))
                .unwrap(),
            )
            .unwrap();
        let err = decoder
            .decode_event(
                serde_json::from_value(serde_json::json!({
                    "type": "response.failed",
                    "response": {
                        "id": "r1",
                        "model": "m1",
                        "status": "failed",
                        "error": {"code": "server_error", "message": "boom", "param": null},
                        "output": []
                    }
                }))
                .unwrap(),
            )
            .unwrap_err();
        match err {
            DecodeError::Failed { message } => assert_eq!(message, "boom"),
            other => panic!("expected Failed, got {other:?}"),
        }
        // 失败后状态机处于 Stopped，后续事件报 DataAfterStop。
        let err = decoder
            .decode_event(
                serde_json::from_value(serde_json::json!({
                    "type": "response.output_text.delta",
                    "output_index": 0,
                    "delta": "x"
                }))
                .unwrap(),
            )
            .unwrap_err();
        assert!(matches!(err, DecodeError::DataAfterStop));
    }

    #[test]
    fn streaming_data_after_finish_errors() {
        let mut decoder = StreamDecoder::new();
        decoder
            .decode_event(
                serde_json::from_value(serde_json::json!({
                    "type": "response.created",
                    "response": {"id": "r1", "model": "m1", "output": []}
                }))
                .unwrap(),
            )
            .unwrap();
        decoder
            .decode_event(
                serde_json::from_value(serde_json::json!({
                    "type": "response.completed",
                    "response": {"id": "r1", "model": "m1", "status": "completed", "output": []}
                }))
                .unwrap(),
            )
            .unwrap();
        let err = decoder
            .decode_event(
                serde_json::from_value(serde_json::json!({
                    "type": "response.output_text.delta",
                    "output_index": 0,
                    "delta": "x"
                }))
                .unwrap(),
            )
            .unwrap_err();
        assert!(matches!(err, DecodeError::DataAfterFinish));
    }

    #[test]
    fn finish_without_start_errors() {
        let mut decoder = StreamDecoder::new();
        let err = decoder.finish().unwrap_err();
        assert!(matches!(err, DecodeError::MissingStart));
    }
}

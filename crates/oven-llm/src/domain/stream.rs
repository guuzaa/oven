//! `Delta` / `StreamEvent`：统一流式事件模型。
//!
//! `StreamEvent` 镜像 Anthropic 的事件粒度；每个 provider 的原生流都被翻译成
//! 这套表示，harness 代码无需关心底层 provider 是谁。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use super::message::{ContentBlock, Role};
use super::response::{Response, StopReason, Usage};

/// `StreamDecoder` 的生命周期阶段。
///
/// - `Initial`：尚未收到 `response.created`，下一个事件会触发 `MessageStart`。
/// - `Streaming`：已发出 `MessageStart`，正在接收内容事件。
/// - `AwaitingDone`：已收到终止事件（`response.completed` /
///   `response.incomplete`）并发出 `MessageDelta`，等待上层调用 `finish()`
///   产出 `MessageStop`。
/// - `Stopped`：已发出 `MessageStop`，流已结束。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamPhase {
    #[default]
    Initial,
    Streaming,
    AwaitingDone,
    Stopped,
}

/// 内容块的增量更新片段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    ThinkingDelta { thinking: String },
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
}

/// 统一流式事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        id: String,
        model: String,
    },
    ContentBlockStart {
        index: usize,
        block: ContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: Delta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        stop_reason: Option<StopReason>,
        usage: Option<Usage>,
    },
    MessageStop,
}

/// [`StreamCollector::finish`] 可能返回的错误。
#[derive(Debug, Error)]
pub enum StreamCollectorError {
    #[error("stream protocol error: {0}")]
    Stream(String),
}

/// 将流式事件累积为一条完整的 [`Response`]。
///
/// 逐条喂入 [`StreamEvent`]，流结束后调用 [`finish`](Self::finish) 拼装成
/// 与 [`Provider::complete`](crate::Provider::complete) 返回的同构 `Response`。
///
/// ```rust
/// # use oven_llm::*;
/// # fn example(stream: impl futures::Stream<Item = std::result::Result<StreamEvent, ProviderError>>) {
/// # futures::executor::block_on(async {
/// use futures::StreamExt;
/// let mut collector = StreamCollector::new();
/// let mut stream = Box::pin(stream);
/// while let Some(event) = stream.next().await {
///     collector.push(&event?);
/// }
/// let response = collector.finish()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # });
/// # }
/// ```
pub struct StreamCollector {
    id: Option<String>,
    model: Option<String>,
    blocks: BTreeMap<usize, ContentBlock>,
    tool_arguments: BTreeMap<usize, String>,
    stop_reason: Option<StopReason>,
    usage: Option<Usage>,
}

impl StreamCollector {
    /// 创建空的收集器。
    pub fn new() -> Self {
        Self {
            id: None,
            model: None,
            blocks: BTreeMap::new(),
            tool_arguments: BTreeMap::new(),
            stop_reason: None,
            usage: None,
        }
    }

    /// 喂入一条流式事件。
    ///
    /// 调用方可在调用前检查事件内容（例如将文本 delta 打印到终端），
    /// 因为参数为 `&StreamEvent`。
    pub fn push(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::MessageStart { id, model } => {
                self.id = Some(id.clone());
                self.model = Some(model.clone());
            }
            StreamEvent::ContentBlockStart { index, block } => {
                if let ContentBlock::ToolUse {
                    input,
                    raw_arguments,
                    ..
                } = block
                {
                    // 起始块可能已经带着 wire 原始文本（有的 provider 一次给全
                    // 参数），否则退回占位 `input`：流式阶段它只是一个空串或已到
                    // 达的参数片段，`InputJsonDelta` 会继续往后拼。
                    let initial_arguments = match raw_arguments {
                        Some(raw) => raw.clone(),
                        None => match input {
                            serde_json::Value::String(arguments) => arguments.clone(),
                            arguments => arguments.to_string(),
                        },
                    };
                    self.tool_arguments.insert(*index, initial_arguments);
                }
                self.blocks.insert(*index, block.clone());
            }
            StreamEvent::ContentBlockDelta { index, delta } => match delta {
                Delta::ThinkingDelta { thinking } => {
                    if let Some(ContentBlock::Thinking {
                        thinking: accumulated,
                    }) = self.blocks.get_mut(index)
                    {
                        accumulated.push_str(thinking);
                    }
                }
                Delta::TextDelta { text } => {
                    if let Some(ContentBlock::Text { text: accumulated }) =
                        self.blocks.get_mut(index)
                    {
                        accumulated.push_str(text);
                    }
                }
                Delta::InputJsonDelta { partial_json } => {
                    if let Some(arguments) = self.tool_arguments.get_mut(index) {
                        arguments.push_str(partial_json);
                    }
                }
            },
            StreamEvent::MessageDelta { stop_reason, usage } => {
                self.stop_reason = *stop_reason;
                self.usage = *usage;
            }
            StreamEvent::ContentBlockStop { .. } | StreamEvent::MessageStop => {}
        }
    }

    /// 将收集到的流式事件拼装为一条完整的 [`Response`]。
    ///
    /// 此方法同步执行，会解析分片的工具参数 JSON。
    pub fn finish(self) -> Result<Response, StreamCollectorError> {
        let id = self
            .id
            .ok_or_else(|| StreamCollectorError::Stream("missing MessageStart event".into()))?;
        let model = self.model.unwrap_or_default();

        let mut blocks = self.blocks;

        for (index, raw_arguments) in self.tool_arguments {
            // 空串（模型没有吐出任何参数）没有可回传的原始文本：`input` 用 `{}`，
            // 由 encoder 退回紧凑序列化。其余情况把原始文本逐字节存进内容块，
            // 让重放路径与模型当时生成的 token 完全一致。
            let (input, raw) = if raw_arguments.trim().is_empty() {
                (json!({}), None)
            } else {
                let input = serde_json::from_str(&raw_arguments).map_err(|error| {
                    StreamCollectorError::Stream(format!(
                        "invalid JSON arguments for tool block {index}: {error}"
                    ))
                })?;
                (input, Some(raw_arguments))
            };
            let Some(ContentBlock::ToolUse {
                input: target,
                raw_arguments: target_raw,
                ..
            }) = blocks.get_mut(&index)
            else {
                return Err(StreamCollectorError::Stream(format!(
                    "tool arguments collected for non-tool block {index}"
                )));
            };
            *target = input;
            *target_raw = raw;
        }

        Ok(Response {
            id,
            model,
            role: Role::Assistant,
            content: blocks.into_values().collect(),
            stop_reason: self.stop_reason,
            usage: self.usage,
        })
    }
}

impl Default for StreamCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_text_serializes_correctly() {
        let delta = Delta::TextDelta {
            text: "hello".to_string(),
        };
        let json = serde_json::to_value(&delta).unwrap();
        assert_eq!(json["type"], "text_delta");
        assert_eq!(json["text"], "hello");
        let decoded: Delta = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, delta);
    }

    #[test]
    fn delta_thinking_serializes_correctly() {
        let delta = Delta::ThinkingDelta {
            thinking: "let me reason...".to_string(),
        };
        let json = serde_json::to_value(&delta).unwrap();
        assert_eq!(json["type"], "thinking_delta");
        assert_eq!(json["thinking"], "let me reason...");
        let decoded: Delta = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, delta);
    }

    #[test]
    fn delta_input_json_round_trips() {
        let delta = Delta::InputJsonDelta {
            partial_json: "{\"a\":1}".to_string(),
        };
        let json = serde_json::to_string(&delta).unwrap();
        let decoded: Delta = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, delta);
    }

    #[test]
    fn stream_event_message_start_serializes_correctly() {
        let event = StreamEvent::MessageStart {
            id: "msg_1".to_string(),
            model: "gpt-4".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "message_start");
        assert_eq!(json["id"], "msg_1");
        assert_eq!(json["model"], "gpt-4");
    }

    #[test]
    fn stream_event_content_block_start_serializes_correctly() {
        let event = StreamEvent::ContentBlockStart {
            index: 0,
            block: ContentBlock::text("hi"),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "content_block_start");
        assert_eq!(json["index"], 0);
        assert_eq!(json["block"]["type"], "text");
        assert_eq!(json["block"]["text"], "hi");
    }

    #[test]
    fn stream_event_content_block_delta_serializes_correctly() {
        let event = StreamEvent::ContentBlockDelta {
            index: 1,
            delta: Delta::TextDelta {
                text: "chunk".to_string(),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "content_block_delta");
        assert_eq!(json["index"], 1);
    }

    #[test]
    fn stream_event_content_block_stop_serializes_correctly() {
        let event = StreamEvent::ContentBlockStop { index: 2 };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "content_block_stop");
        assert_eq!(json["index"], 2);
    }

    #[test]
    fn stream_event_message_delta_serializes_with_optional_fields() {
        let event = StreamEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            usage: Some(Usage {
                input_tokens: 3,
                output_tokens: 4,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
            }),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "message_delta");
        assert_eq!(json["stop_reason"], "end_turn");
        assert_eq!(json["usage"]["input_tokens"], 3);
        assert_eq!(json["usage"]["output_tokens"], 4);
    }

    #[test]
    fn stream_event_message_delta_allows_none_fields() {
        let event = StreamEvent::MessageDelta {
            stop_reason: None,
            usage: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["stop_reason"], serde_json::Value::Null);
        assert_eq!(json["usage"], serde_json::Value::Null);
    }

    #[test]
    fn stream_event_message_stop_serializes_correctly() {
        let event = StreamEvent::MessageStop;
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "message_stop");
    }

    #[test]
    fn stream_event_round_trips_through_json() {
        let event = StreamEvent::ContentBlockDelta {
            index: 5,
            delta: Delta::InputJsonDelta {
                partial_json: "{\"x\":1}".to_string(),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: StreamEvent = serde_json::from_str(&json).unwrap();
        match decoded {
            StreamEvent::ContentBlockDelta { index, delta } => {
                assert_eq!(index, 5);
                assert_eq!(
                    delta,
                    Delta::InputJsonDelta {
                        partial_json: "{\"x\":1}".to_string()
                    }
                );
            }
            _ => panic!("expected ContentBlockDelta"),
        }
    }

    #[test]
    fn collector_text_only_stream() {
        let mut c = StreamCollector::new();
        c.push(&StreamEvent::MessageStart {
            id: "msg_1".into(),
            model: "gpt-4".into(),
        });
        c.push(&StreamEvent::ContentBlockStart {
            index: 0,
            block: ContentBlock::Text {
                text: String::new(),
            },
        });
        c.push(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: Delta::TextDelta {
                text: "Hello".into(),
            },
        });
        c.push(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: Delta::TextDelta {
                text: " world".into(),
            },
        });
        c.push(&StreamEvent::ContentBlockStop { index: 0 });
        c.push(&StreamEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
            }),
        });
        c.push(&StreamEvent::MessageStop);

        let resp = c.finish().unwrap();
        assert_eq!(resp.id, "msg_1");
        assert_eq!(resp.model, "gpt-4");
        assert_eq!(resp.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(resp.usage.unwrap().input_tokens, 10);
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello world"),
            _ => panic!("expected Text block"),
        }
    }

    #[test]
    fn collector_thinking_and_text_blocks() {
        let mut c = StreamCollector::new();
        c.push(&StreamEvent::MessageStart {
            id: "msg_2".into(),
            model: "claude-3".into(),
        });
        c.push(&StreamEvent::ContentBlockStart {
            index: 0,
            block: ContentBlock::Thinking {
                thinking: String::new(),
            },
        });
        c.push(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: Delta::ThinkingDelta {
                thinking: "let me".into(),
            },
        });
        c.push(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: Delta::ThinkingDelta {
                thinking: " think".into(),
            },
        });
        c.push(&StreamEvent::ContentBlockStart {
            index: 1,
            block: ContentBlock::Text {
                text: String::new(),
            },
        });
        c.push(&StreamEvent::ContentBlockDelta {
            index: 1,
            delta: Delta::TextDelta {
                text: "answer".into(),
            },
        });
        c.push(&StreamEvent::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        });

        let resp = c.finish().unwrap();
        assert_eq!(resp.content.len(), 2);
        match &resp.content[0] {
            ContentBlock::Thinking { thinking } => assert_eq!(thinking, "let me think"),
            _ => panic!("expected Thinking block"),
        }
        match &resp.content[1] {
            ContentBlock::Text { text } => assert_eq!(text, "answer"),
            _ => panic!("expected Text block"),
        }
    }

    #[test]
    fn collector_tool_use_with_json_arguments() {
        let mut c = StreamCollector::new();
        c.push(&StreamEvent::MessageStart {
            id: "msg_3".into(),
            model: "gpt-4".into(),
        });
        c.push(&StreamEvent::ContentBlockStart {
            index: 0,
            block: ContentBlock::tool_use(
                "tool_1",
                "read_file",
                serde_json::Value::String(String::new()),
            ),
        });
        c.push(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: Delta::InputJsonDelta {
                partial_json: "{\"path\":".into(),
            },
        });
        c.push(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: Delta::InputJsonDelta {
                partial_json: "\"src/main.rs\"}".into(),
            },
        });
        c.push(&StreamEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        });

        let resp = c.finish().unwrap();
        assert_eq!(resp.stop_reason, Some(StopReason::ToolUse));
        assert_eq!(resp.content.len(), 1);
        match &resp.content[0] {
            ContentBlock::ToolUse {
                id, name, input, ..
            } => {
                assert_eq!(id, "tool_1");
                assert_eq!(name, "read_file");
                assert_eq!(input["path"], "src/main.rs");
            }
            _ => panic!("expected ToolUse block"),
        }
    }

    /// deepseek 生成的是 `{"location": "Hangzhou"}`（冒号后有空格）。重放必须
    /// 逐字节保留这段文本：provider 的隐式缓存按「完整匹配缓存单元」判定，重新
    /// 序列化会让「模型输出结束位置」的单元永远失配。
    #[test]
    fn collector_preserves_raw_tool_arguments_byte_for_byte() {
        let raw = r#"{"location": "Hangzhou, Zhejiang"}"#;
        let mut c = StreamCollector::new();
        c.push(&StreamEvent::MessageStart {
            id: "msg_raw".into(),
            model: "deepseek-flash".into(),
        });
        c.push(&StreamEvent::ContentBlockStart {
            index: 0,
            block: ContentBlock::tool_use(
                "call_1",
                "probe",
                serde_json::Value::String(String::new()),
            ),
        });
        // 与真实 SSE 一致：参数是分片到达的。
        let fragments = [
            "{",
            r#""location""#,
            r#": "#,
            r#""Hang"#,
            "zhou",
            r#", Zhejiang""#,
            "}",
        ];
        assert_eq!(fragments.concat(), raw);
        for fragment in fragments {
            c.push(&StreamEvent::ContentBlockDelta {
                index: 0,
                delta: Delta::InputJsonDelta {
                    partial_json: fragment.into(),
                },
            });
        }
        c.push(&StreamEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        });

        let resp = c.finish().unwrap();
        match &resp.content[0] {
            ContentBlock::ToolUse {
                input,
                raw_arguments,
                ..
            } => {
                assert_eq!(raw_arguments.as_deref(), Some(raw));
                assert_eq!(input["location"], "Hangzhou, Zhejiang");
            }
            other => panic!("expected ToolUse block, got {other:?}"),
        }
    }

    /// 起始块已经带着 wire 原始文本时，以它为准（部分 provider 一次给全参数）。
    #[test]
    fn collector_prefers_raw_arguments_from_block_start() {
        let raw = r#"{ "path": "a.rs" }"#;
        let mut c = StreamCollector::new();
        c.push(&StreamEvent::MessageStart {
            id: "msg_start_raw".into(),
            model: "gpt-4".into(),
        });
        c.push(&StreamEvent::ContentBlockStart {
            index: 0,
            block: ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "read_file".into(),
                input: serde_json::json!({ "path": "a.rs" }),
                raw_arguments: Some(raw.into()),
            },
        });
        c.push(&StreamEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        });

        let resp = c.finish().unwrap();
        match &resp.content[0] {
            ContentBlock::ToolUse { raw_arguments, .. } => {
                assert_eq!(raw_arguments.as_deref(), Some(raw));
            }
            other => panic!("expected ToolUse block, got {other:?}"),
        }
    }

    /// 模型一个参数都没吐时没有可回传的原文，交给 encoder 紧凑序列化 `{}`。
    #[test]
    fn collector_empty_arguments_leave_no_raw_text() {
        let mut c = StreamCollector::new();
        c.push(&StreamEvent::MessageStart {
            id: "msg_empty_args".into(),
            model: "gpt-4".into(),
        });
        c.push(&StreamEvent::ContentBlockStart {
            index: 0,
            block: ContentBlock::tool_use(
                "call_1",
                "noop",
                serde_json::Value::String(String::new()),
            ),
        });
        c.push(&StreamEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        });

        let resp = c.finish().unwrap();
        match &resp.content[0] {
            ContentBlock::ToolUse {
                input,
                raw_arguments,
                ..
            } => {
                assert_eq!(raw_arguments, &None);
                assert_eq!(input, &serde_json::json!({}));
            }
            other => panic!("expected ToolUse block, got {other:?}"),
        }
    }

    #[test]
    fn collector_missing_message_start_errors() {
        let c = StreamCollector::new();
        let err = c.finish().unwrap_err();
        assert!(err.to_string().contains("missing MessageStart"));
    }

    #[test]
    fn collector_invalid_tool_json_errors() {
        let mut c = StreamCollector::new();
        c.push(&StreamEvent::MessageStart {
            id: "msg_4".into(),
            model: "gpt-4".into(),
        });
        c.push(&StreamEvent::ContentBlockStart {
            index: 0,
            block: ContentBlock::tool_use(
                "tool_1",
                "test",
                serde_json::Value::String(String::new()),
            ),
        });
        c.push(&StreamEvent::ContentBlockDelta {
            index: 0,
            delta: Delta::InputJsonDelta {
                partial_json: "not json".into(),
            },
        });
        c.push(&StreamEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        });

        let err = c.finish().unwrap_err();
        assert!(err.to_string().contains("invalid JSON arguments"));
    }

    #[test]
    fn collector_default_is_same_as_new() {
        let c = StreamCollector::default();
        assert!(c.id.is_none());
        assert!(c.blocks.is_empty());
    }
}

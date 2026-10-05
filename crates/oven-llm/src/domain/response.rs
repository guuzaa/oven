//! `Usage` / `StopReason` / `Response`：provider 无关的非流式响应模型。

use std::ops::{Add, AddAssign, Sub, SubAssign};

use serde::{Deserialize, Serialize};

use super::message::{ContentBlock, Role};

/// 一次调用的 token 用量统计。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_read_tokens: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reasoning_tokens: u32,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

impl Usage {
    pub fn reset(&mut self) {
        *self = Default::default();
    }
}

impl Sub for Usage {
    type Output = Usage;
    fn sub(self, rhs: Self) -> Self::Output {
        Usage {
            input_tokens: self.input_tokens.saturating_sub(rhs.input_tokens),
            output_tokens: self.output_tokens.saturating_sub(rhs.output_tokens),
            cache_read_tokens: self.cache_read_tokens.saturating_sub(rhs.cache_read_tokens),
            reasoning_tokens: self.reasoning_tokens.saturating_sub(rhs.reasoning_tokens),
        }
    }
}

impl SubAssign for Usage {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl Add for Usage {
    type Output = Usage;

    fn add(self, rhs: Usage) -> Usage {
        Usage {
            input_tokens: self.input_tokens.saturating_add(rhs.input_tokens),
            output_tokens: self.output_tokens.saturating_add(rhs.output_tokens),
            cache_read_tokens: self.cache_read_tokens.saturating_add(rhs.cache_read_tokens),
            reasoning_tokens: self.reasoning_tokens.saturating_add(rhs.reasoning_tokens),
        }
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, rhs: Usage) {
        *self = *self + rhs;
    }
}

/// 响应终止原因，与具体 provider 无关。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// 模型自然结束本轮生成。
    EndTurn,
    /// 命中了请求中指定的停止序列。
    StopSequence,
    /// 模型请求调用工具。
    ToolUse,
    /// 达到了 `max_tokens` 限制。
    MaxTokens,
}

/// 一次 LLM 调用的完整非流式响应，与具体 provider 无关。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: String,
    pub model: String,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<StopReason>,
    pub usage: Option<Usage>,
}

impl Response {
    /// 按出现顺序拼接所有 `Text` 内容块。
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// 按出现顺序拼接所有 `Thinking` 内容块。
    pub fn thinking(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Thinking { thinking } => Some(thinking.as_str()),
                _ => None,
            })
            .collect()
    }

    /// 迭代响应中的所有 `ToolUse` 内容块。
    pub fn tool_uses(&self) -> impl Iterator<Item = &ContentBlock> + '_ {
        self.content
            .iter()
            .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
    }

    /// 响应是否包含至少一个 `ToolUse` 内容块。
    pub fn has_tool_use(&self) -> bool {
        self.tool_uses().next().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_default_is_zero() {
        let usage = Usage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.reasoning_tokens, 0);
    }

    #[test]
    fn usage_reset_fields() {
        let mut usage = Usage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 1,
            reasoning_tokens: 2,
        };
        usage.reset();
        assert_eq!(usage, Usage::default());
    }

    #[test]
    fn usage_serializes_fields() {
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
        };
        let json = serde_json::to_value(usage).unwrap();
        assert_eq!(json["input_tokens"], 10);
        assert_eq!(json["output_tokens"], 20);
        assert!(json.get("cache_read_tokens").is_none());
        assert!(json.get("reasoning_tokens").is_none());
    }

    #[test]
    fn usage_serializes_cache_read_tokens_when_nonzero() {
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 5,
            reasoning_tokens: 0,
        };
        let json = serde_json::to_value(usage).unwrap();
        assert_eq!(json["cache_read_tokens"], 5);
    }

    #[test]
    fn usage_serializes_reasoning_tokens_when_nonzero() {
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 0,
            reasoning_tokens: 8,
        };
        let json = serde_json::to_value(usage).unwrap();
        assert_eq!(json["reasoning_tokens"], 8);
    }

    #[test]
    fn usage_add_sub_all_fields() {
        let a = Usage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 3,
            reasoning_tokens: 4,
        };
        let b = Usage {
            input_tokens: 5,
            output_tokens: 7,
            cache_read_tokens: 1,
            reasoning_tokens: 2,
        };
        assert_eq!(
            a + b,
            Usage {
                input_tokens: 15,
                output_tokens: 27,
                cache_read_tokens: 4,
                reasoning_tokens: 6,
            }
        );
        assert_eq!(
            a - b,
            Usage {
                input_tokens: 5,
                output_tokens: 13,
                cache_read_tokens: 2,
                reasoning_tokens: 2,
            }
        )
    }

    #[test]
    fn usage_add_sub_assign_accumulates() {
        let mut total = Usage::default();
        total += Usage {
            input_tokens: 1,
            output_tokens: 2,
            cache_read_tokens: 3,
            reasoning_tokens: 4,
        };
        total += Usage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 30,
            reasoning_tokens: 40,
        };
        assert_eq!(
            total,
            Usage {
                input_tokens: 11,
                output_tokens: 22,
                cache_read_tokens: 33,
                reasoning_tokens: 44,
            }
        );
        total -= Usage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 30,
            reasoning_tokens: 40,
        };
        assert_eq!(
            total,
            Usage {
                input_tokens: 1,
                output_tokens: 2,
                cache_read_tokens: 3,
                reasoning_tokens: 4,
            }
        );
    }

    #[test]
    fn usage_add_saturates_on_overflow() {
        let a = Usage {
            input_tokens: u32::MAX,
            output_tokens: u32::MAX - 1,
            cache_read_tokens: u32::MAX,
            reasoning_tokens: 0,
        };
        let b = Usage {
            input_tokens: 1,
            output_tokens: 5,
            cache_read_tokens: 0,
            reasoning_tokens: u32::MAX,
        };
        assert_eq!(
            a + b,
            Usage {
                input_tokens: u32::MAX,
                output_tokens: u32::MAX,
                cache_read_tokens: u32::MAX,
                reasoning_tokens: u32::MAX,
            }
        );
    }

    #[test]
    fn stop_reason_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&StopReason::EndTurn).unwrap(),
            "\"end_turn\""
        );
        assert_eq!(
            serde_json::to_string(&StopReason::StopSequence).unwrap(),
            "\"stop_sequence\""
        );
        assert_eq!(
            serde_json::to_string(&StopReason::ToolUse).unwrap(),
            "\"tool_use\""
        );
        assert_eq!(
            serde_json::to_string(&StopReason::MaxTokens).unwrap(),
            "\"max_tokens\""
        );
    }

    #[test]
    fn stop_reason_round_trips() {
        for reason in [
            StopReason::EndTurn,
            StopReason::StopSequence,
            StopReason::ToolUse,
            StopReason::MaxTokens,
        ] {
            let json = serde_json::to_string(&reason).unwrap();
            let decoded: StopReason = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, reason);
        }
    }

    #[test]
    fn response_serializes_with_optional_fields_present() {
        let response = Response {
            id: "msg_1".to_string(),
            model: "gpt-4".to_string(),
            role: Role::Assistant,
            content: vec![ContentBlock::text("hi")],
            stop_reason: Some(StopReason::EndTurn),
            usage: Some(Usage {
                input_tokens: 5,
                output_tokens: 7,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
            }),
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["id"], "msg_1");
        assert_eq!(json["model"], "gpt-4");
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["stop_reason"], "end_turn");
        assert_eq!(json["usage"]["input_tokens"], 5);
        assert_eq!(json["usage"]["output_tokens"], 7);
    }

    #[test]
    fn response_allows_none_stop_reason_and_usage() {
        let response = Response {
            id: "msg_2".to_string(),
            model: "gpt-4".to_string(),
            role: Role::Assistant,
            content: vec![],
            stop_reason: None,
            usage: None,
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["stop_reason"], serde_json::Value::Null);
        assert_eq!(json["usage"], serde_json::Value::Null);
    }

    #[test]
    fn response_round_trips_through_json() {
        let response = Response {
            id: "msg_3".to_string(),
            model: "claude-3".to_string(),
            role: Role::Assistant,
            content: vec![ContentBlock::text("42")],
            stop_reason: Some(StopReason::ToolUse),
            usage: Some(Usage {
                input_tokens: 1,
                output_tokens: 2,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
            }),
        };
        let json = serde_json::to_string(&response).unwrap();
        let decoded: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.id, response.id);
        assert_eq!(decoded.model, response.model);
        assert_eq!(decoded.role, response.role);
        assert_eq!(decoded.stop_reason, response.stop_reason);
        assert_eq!(decoded.usage, response.usage);
    }

    fn sample_response(content: Vec<ContentBlock>) -> Response {
        Response {
            id: "msg".into(),
            model: "m".into(),
            role: Role::Assistant,
            content,
            stop_reason: None,
            usage: None,
        }
    }

    #[test]
    fn response_text_concatenates_text_blocks_in_order() {
        let response = sample_response(vec![
            ContentBlock::thinking("skip"),
            ContentBlock::text("Hello"),
            ContentBlock::text(" "),
            ContentBlock::text("world"),
            ContentBlock::tool_use("t1", "f", serde_json::json!({})),
        ]);
        assert_eq!(response.text(), "Hello world");
    }

    #[test]
    fn response_text_empty_when_no_text_blocks() {
        let response = sample_response(vec![ContentBlock::thinking("only thinking")]);
        assert_eq!(response.text(), "");
    }

    #[test]
    fn response_thinking_concatenates_thinking_blocks_in_order() {
        let response = sample_response(vec![
            ContentBlock::thinking("let me "),
            ContentBlock::text("ignore"),
            ContentBlock::thinking("think"),
            ContentBlock::tool_use("t1", "f", serde_json::json!({})),
        ]);
        assert_eq!(response.thinking(), "let me think");
    }

    #[test]
    fn response_thinking_empty_when_no_thinking_blocks() {
        let response = sample_response(vec![ContentBlock::text("hi")]);
        assert_eq!(response.thinking(), "");
    }

    #[test]
    fn response_tool_uses_iterates_only_tool_use_blocks() {
        let response = sample_response(vec![
            ContentBlock::text("hi"),
            ContentBlock::tool_use("t1", "a", serde_json::json!({"x": 1})),
            ContentBlock::thinking("..."),
            ContentBlock::tool_use("t2", "b", serde_json::json!({"y": 2})),
        ]);
        let uses: Vec<_> = response.tool_uses().collect();
        assert_eq!(uses.len(), 2);
        match uses[0] {
            ContentBlock::ToolUse { id, name, .. } => {
                assert_eq!(id, "t1");
                assert_eq!(name, "a");
            }
            _ => panic!("expected ToolUse"),
        }
        match uses[1] {
            ContentBlock::ToolUse { id, name, .. } => {
                assert_eq!(id, "t2");
                assert_eq!(name, "b");
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn response_has_tool_use() {
        let with = sample_response(vec![ContentBlock::tool_use(
            "t1",
            "f",
            serde_json::json!({}),
        )]);
        let without = sample_response(vec![ContentBlock::text("hi")]);
        assert!(with.has_tool_use());
        assert!(!without.has_tool_use());
    }
}

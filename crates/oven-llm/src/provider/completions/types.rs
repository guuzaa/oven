//! OpenAI Chat Completions 的 wire 格式类型。

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 请求侧类型
// ---------------------------------------------------------------------------

/// OpenAI Chat Completions 请求体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<WireTool>>,
    /// `"auto"` / `"required"` / `"none"` 或
    /// `{"type": "function", "function": {"name": ...}}`——形状随
    /// `ToolChoice` 变体而变化，故使用 `serde_json::Value` 承载。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<StopValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<WireThinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
}

/// `SamplingParams.stop` 的 wire 表示：单个字符串或字符串数组
/// （Requirements 3.6）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub(crate) enum StopValue {
    Single(String),
    Multiple(Vec<String>),
}

/// `thinking` 参数的 wire 包装：`{"type": "enabled"}` / `{"type": "disabled"}`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WireThinking {
    #[serde(rename = "type")]
    pub mode: String,
}

/// 流式请求下用于要求服务端在最后一个 chunk 携带 `usage` 字段。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct StreamOptions {
    pub include_usage: bool,
}

/// 请求侧的一条消息：`role` 取值为 `"system"` / `"user"` / `"assistant"` /
/// `"tool"`，具体取值范围由 encoder 保证。
///
/// `reasoning_content` 仅 `role: "assistant"` 消息携带：回传上一轮的思考
/// 内容，保证多轮对话（尤其是交错式思考）与 KV cache 前缀一致。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub(crate) struct WireMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<WireToolCall>>,
    /// 仅 `role: "tool"` 消息携带：对应触发该结果的 `tool_calls[].id`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// 兼容部分 provider 对 `role: "tool"` 消息要求携带的工具名。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 请求侧的工具定义：`{"type": "function", "function": {...}}`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WireTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: WireFunction,
}

/// `WireTool.function` 字段：工具名 / 描述 / JSON Schema 参数。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WireFunction {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

/// 请求侧（`assistant` 消息内）的工具调用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WireToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: WireToolCallFunction,
}

/// `WireToolCall.function` 字段：`arguments` 是 JSON 编码后的字符串
/// （非嵌套 JSON 值），与 OpenAI wire 格式一致。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WireToolCallFunction {
    pub name: String,
    pub arguments: String,
}

// ---------------------------------------------------------------------------
// 非流式响应类型
// ---------------------------------------------------------------------------

/// OpenAI Chat Completions 非流式响应体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ChatCompletionResponse {
    pub id: String,
    pub model: String,
    pub choices: Vec<WireChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<WireUsage>,
}

/// 非流式响应中的一个 choice。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WireChoice {
    pub index: u32,
    pub message: WireResponseMessage,
    pub finish_reason: Option<String>,
}

/// 非流式响应中的消息体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub(crate) struct WireResponseMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<WireResponseToolCall>>,
}

/// 非流式响应中的工具调用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WireResponseToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: WireResponseToolCallFunction,
}

/// `WireResponseToolCall.function` 字段：`arguments` 为 JSON 编码后的
/// 字符串，需要 decoder 再次 `serde_json::from_str` 解析
/// （Requirements 4.4）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WireResponseToolCallFunction {
    pub name: String,
    pub arguments: String,
}

/// token 用量统计（Requirement 4.6）。
///
/// `prompt_tokens` / `completion_tokens` / `total_tokens` 是 OpenAI 标准
/// 字段，所有兼容 provider 都会返回。其余字段均为各家扩展、用于统计 KV
/// cache 命中数：
/// - `prompt_tokens_details.cached_tokens`：OpenAI / zhipu / deepseek /
///   kimi 均提供，是最稳定的统一来源；
/// - `cached_tokens`（顶层）：kimi 额外冗余提供；
/// - `prompt_cache_hit_tokens`：deepseek 额外冗余提供。
///
/// decoder 按 `prompt_tokens_details.cached_tokens` → `cached_tokens` →
/// `prompt_cache_hit_tokens` 的优先级取首个非零值填入 `Usage.cache_read_tokens`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct WireUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<WireTokenDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens_details: Option<WireTokenDetails>,
    /// kimi 顶层冗余字段，等价于 `prompt_tokens_details.cached_tokens`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
    /// deepseek 冗余字段，等价于 `prompt_tokens_details.cached_tokens`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_hit_tokens: Option<u32>,
    /// deepseek 冗余字段：未命中 KV cache 的 prompt token 数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_miss_tokens: Option<u32>,
}

/// `*_tokens_details` 子结构：`cached_tokens` 出现在 `prompt_tokens_details`
/// 表示命中 KV cache 的输入 token 数；出现在 `completion_tokens_details`
/// 表示推理 token 数（reasoning_tokens）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct WireTokenDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// 流式 chunk 类型
// ---------------------------------------------------------------------------

/// OpenAI Chat Completions 流式响应的单个 SSE `data` chunk。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ChatCompletionChunk {
    pub id: String,
    pub model: String,
    #[serde(default)]
    pub choices: Vec<WireStreamChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<WireUsage>,
}

/// 流式 chunk 中的一个 choice。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WireStreamChoice {
    pub index: u32,
    pub delta: WireStreamDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

/// 流式 chunk 的增量字段：三者都可选，因为同一逻辑消息的不同字段会分散在
/// 不同 chunk 中逐步到达。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub(crate) struct WireStreamDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<WireStreamToolCall>>,
}

/// 流式 chunk 中的工具调用增量：由 `index` 标识该增量归属于哪个工具调用块
/// （同一 `index` 的多个 chunk 携带同一工具调用的不同片段）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WireStreamToolCall {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<WireStreamToolCallFunction>,
}

/// `WireStreamToolCall.function` 字段：`name` 通常只在首个片段出现，
/// `arguments` 是逐步拼接的 JSON 字符串片段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub(crate) struct WireStreamToolCallFunction {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_value_single_serializes_as_string() {
        let stop = StopValue::Single("STOP".to_string());
        assert_eq!(serde_json::to_value(&stop).unwrap(), "STOP");
    }

    #[test]
    fn stop_value_multiple_serializes_as_array() {
        let stop = StopValue::Multiple(vec!["A".to_string(), "B".to_string()]);
        let json = serde_json::to_value(&stop).unwrap();
        assert_eq!(json, serde_json::json!(["A", "B"]));
    }

    #[test]
    fn chat_completion_request_skips_none_optional_fields() {
        let req = ChatCompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![],
            tools: None,
            tool_choice: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop: None,
            thinking: None,
            reasoning_effort: None,
            stream: false,
            stream_options: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("tools").is_none());
        assert!(json.get("tool_choice").is_none());
        assert!(json.get("temperature").is_none());
        assert!(json.get("top_p").is_none());
        assert!(json.get("max_tokens").is_none());
        assert!(json.get("stop").is_none());
        assert!(json.get("thinking").is_none());
        assert!(json.get("reasoning_effort").is_none());
        assert!(json.get("stream_options").is_none());
        assert_eq!(json["stream"], false);
    }

    #[test]
    fn wire_tool_kind_serializes_as_type_field() {
        let tool = WireTool {
            kind: "function".to_string(),
            function: WireFunction {
                name: "get_weather".to_string(),
                description: None,
                parameters: serde_json::json!({}),
            },
        };
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["type"], "function");
        assert!(json["function"].get("description").is_none());
    }

    #[test]
    fn wire_tool_call_round_trips() {
        let call = WireToolCall {
            id: "call_1".to_string(),
            kind: "function".to_string(),
            function: WireToolCallFunction {
                name: "get_weather".to_string(),
                arguments: "{\"city\":\"Beijing\"}".to_string(),
            },
        };
        let json = serde_json::to_string(&call).unwrap();
        let decoded: WireToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, call);
    }

    #[test]
    fn chat_completion_response_decodes_from_openai_shape() {
        let json = serde_json::json!({
            "id": "chatcmpl-1",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "hi",
                    "tool_calls": null
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });
        let response: ChatCompletionResponse = serde_json::from_value(json).unwrap();
        assert_eq!(response.id, "chatcmpl-1");
        assert_eq!(response.choices.len(), 1);
        assert_eq!(response.choices[0].message.role, "assistant");
        assert_eq!(response.choices[0].finish_reason, Some("stop".to_string()));
        let usage = response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn chat_completion_chunk_decodes_with_stream_tool_call_delta() {
        let json = serde_json::json!({
            "id": "chatcmpl-1",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "get_weather", "arguments": "{\"c" }
                    }]
                },
                "finish_reason": null
            }]
        });
        let chunk: ChatCompletionChunk = serde_json::from_value(json).unwrap();
        assert_eq!(chunk.choices.len(), 1);
        let tool_calls = chunk.choices[0].delta.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls[0].index, 0);
        assert_eq!(tool_calls[0].id, Some("call_1".to_string()));
        assert_eq!(tool_calls[0].kind, Some("function".to_string()));
        let function = tool_calls[0].function.as_ref().unwrap();
        assert_eq!(function.name, Some("get_weather".to_string()));
        assert_eq!(function.arguments, Some("{\"c".to_string()));
    }

    #[test]
    fn chat_completion_chunk_defaults_choices_to_empty_when_missing() {
        let json = serde_json::json!({
            "id": "chatcmpl-1",
            "model": "gpt-4",
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 2,
                "total_tokens": 3
            }
        });
        let chunk: ChatCompletionChunk = serde_json::from_value(json).unwrap();
        assert!(chunk.choices.is_empty());
        assert!(chunk.usage.is_some());
    }
}

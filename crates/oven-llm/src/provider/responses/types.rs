//! OpenAI Responses API 的 wire 格式类型。
//!
//! 这些类型仅在 `responses` 模块内部（encoder/decoder/provider）使用，不是
//! crate 的公开 API 的一部分；字段命名与序列化行为对齐 OpenAI Responses API
//! 的实际 wire 格式。

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 请求侧类型
// ---------------------------------------------------------------------------

/// OpenAI Responses API 请求体（`POST /responses`）。
///
/// 所有可选字段均带 `skip_serializing_if`，未设置时不会出现在 wire JSON 中。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ResponseRequest {
    pub model: String,
    /// 顶层 system 提示词（来自 `Request.system`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// 输入项数组：message / function_call / function_call_output。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<ResponseInputItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ResponseTool>>,
    /// `"none"` / `"required"` 或 `{"type":"function","name":...}`；
    /// `Auto` 时整个字段省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// 仅在有 `reasoning_effort` 或 `thinking == Disabled` 时输出，
    /// 形状为 `{"effort": "none"|"low"|"medium"|"high"}`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<WireReasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    pub stream: bool,
}

/// `reasoning` 参数的 wire 包装：`{"effort": "none"|"low"|"medium"|"high"}`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct WireReasoning {
    pub effort: String,
}

/// 请求侧的一个输入项。已知类型为 message / function_call /
/// function_call_output；其余输入项类型（例如 `file`、`web_search_call`）
/// 由 `Other` 变体原样兜底。
///
/// 使用内部 tag（`type` 字段）的 struct 变体：serde 会在反序列化时把 tag
/// 字段从变体内容中移除，因此变体字段中不能出现与 tag 重名的 `type` 字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponseInputItem {
    Message {
        role: String,
        content: InputMessageContent,
    },
    FunctionCall {
        call_id: String,
        name: String,
        /// JSON 编码后的参数字符串（非嵌套 JSON 值），与 Responses API wire
        /// 格式一致。
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
    #[serde(untagged)]
    Other(serde_json::Value),
}

/// message 输入项的 `content`：纯文本时是字符串；多部分（文本 + 图片等）时
/// 是内容部分数组。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub(crate) enum InputMessageContent {
    Text(String),
    Parts(Vec<InputContentPart>),
}

/// message 输入项的内容部分：`input_text` / `input_image`（assistant 回传
/// 文本时使用 `output_text`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum InputContentPart {
    InputText { text: String },
    InputImage { image_url: String },
    OutputText { text: String },
}

/// 请求侧工具定义：`{"type":"function","name":...,"parameters":{...}}`。
///
/// 工具 schema 键统一使用 `parameters`（与 OpenAI 官方 / Grok 一致；
/// DeepSeek 也接受该键）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ResponseTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

// ---------------------------------------------------------------------------
// 非流式响应类型
// ---------------------------------------------------------------------------

/// OpenAI Responses API 非流式响应体（`ResponseObject`）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct ResponseObject {
    pub id: String,
    pub model: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub error: Option<WireError>,
    #[serde(default)]
    pub incomplete_details: Option<WireIncompleteDetails>,
    #[serde(default)]
    pub output: Vec<ResponseOutputItem>,
    #[serde(default)]
    pub usage: Option<WireUsage>,
}

/// `response.error` 字段：非空表示本次请求失败。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WireError {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub param: Option<String>,
}

/// `response.incomplete_details` 字段：`reason` 说明未完成原因（例如
/// `"max_output_tokens"`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WireIncompleteDetails {
    #[serde(default)]
    pub reason: Option<String>,
}

/// 响应侧的一个输出项。已知类型为 message / reasoning / function_call /
/// web_search_call；其余输出项类型由 `Other` 变体原样兜底（decoder 跳过）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ResponseOutputItem {
    Message {
        id: String,
        role: String,
        #[serde(default)]
        content: Vec<WireOutputContentPart>,
    },
    Reasoning {
        id: String,
        #[serde(default)]
        summary: Vec<WireSummaryText>,
    },
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        /// JSON 编码后的参数字符串。
        #[serde(default)]
        arguments: String,
    },
    WebSearchCall {
        id: String,
    },
    #[serde(untagged)]
    Other(serde_json::Value),
}

/// message 输出项的内容部分：`output_text`（其他类型如 `refusal` 忽略）。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum WireOutputContentPart {
    OutputText {
        text: String,
    },
    #[serde(untagged)]
    Other(serde_json::Value),
}

/// reasoning 输出项中的一条摘要文本。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct WireSummaryText {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

/// 响应侧 token 用量统计（Responses API 形态）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct WireUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens_details: Option<WireInputTokensDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens_details: Option<WireOutputTokensDetails>,
}

/// `input_tokens_details` 子结构：`cached_tokens` 表示命中 KV cache 的输入
/// token 数。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct WireInputTokensDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
}

/// `output_tokens_details` 子结构：`reasoning_tokens` 表示推理 token 数。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub(crate) struct WireOutputTokensDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// 流式事件类型
// ---------------------------------------------------------------------------

/// Responses API SSE 流式事件。
///
/// 所有已知事件变体均带显式 `type` 重命名（`response.created` 等）；
/// `sequence_number` 字段反序列化但被忽略。未知事件类型由 `Other` 兜底，
/// decoder 对 `Other` 不做任何处理。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub(crate) enum ResponseEvent {
    #[serde(rename = "response.created")]
    ResponseCreated { response: ResponseObject },
    #[serde(rename = "response.in_progress")]
    ResponseInProgress { response: ResponseObject },
    #[serde(rename = "response.output_item.added")]
    ResponseOutputItemAdded {
        output_index: u64,
        item: ResponseOutputItem,
    },
    #[serde(rename = "response.output_item.done")]
    ResponseOutputItemDone {
        output_index: u64,
        item: ResponseOutputItem,
    },
    #[serde(rename = "response.content_part.added")]
    ResponseContentPartAdded {
        output_index: u64,
        #[allow(dead_code)]
        part: serde_json::Value,
    },
    #[serde(rename = "response.content_part.done")]
    ResponseContentPartDone {
        output_index: u64,
        #[allow(dead_code)]
        part: serde_json::Value,
    },
    #[serde(rename = "response.output_text.delta")]
    ResponseOutputTextDelta { output_index: u64, delta: String },
    #[serde(rename = "response.output_text.done")]
    ResponseOutputTextDone { output_index: u64 },
    #[serde(rename = "response.reasoning_text.delta")]
    ResponseReasoningTextDelta { output_index: u64, delta: String },
    #[serde(rename = "response.reasoning_text.done")]
    ResponseReasoningTextDone { output_index: u64 },
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ResponseReasoningSummaryTextDelta { output_index: u64, delta: String },
    #[serde(rename = "response.reasoning_summary_text.done")]
    ResponseReasoningSummaryTextDone { output_index: u64 },
    #[serde(rename = "response.function_call_arguments.delta")]
    ResponseFunctionCallArgumentsDelta { output_index: u64, delta: String },
    #[serde(rename = "response.function_call_arguments.done")]
    ResponseFunctionCallArgumentsDone { output_index: u64 },
    #[serde(rename = "response.completed")]
    ResponseCompleted { response: ResponseObject },
    #[serde(rename = "response.incomplete")]
    ResponseIncomplete { response: ResponseObject },
    #[serde(rename = "response.failed")]
    ResponseFailed { response: ResponseObject },
    #[serde(untagged)]
    Other(serde_json::Value),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_response_request_skips_none_optional_fields() {
        let req = ResponseRequest {
            model: "gpt-4o".to_string(),
            instructions: None,
            input: None,
            tools: None,
            tool_choice: None,
            reasoning: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            stream: false,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("instructions").is_none());
        assert!(json.get("input").is_none());
        assert!(json.get("tools").is_none());
        assert!(json.get("tool_choice").is_none());
        assert!(json.get("reasoning").is_none());
        assert!(json.get("temperature").is_none());
        assert!(json.get("top_p").is_none());
        assert!(json.get("max_output_tokens").is_none());
        assert_eq!(json["model"], "gpt-4o");
        assert_eq!(json["stream"], false);
    }

    #[test]
    fn input_item_unknown_type_falls_back_to_other() {
        let item: ResponseInputItem =
            serde_json::from_value(serde_json::json!({"type": "file", "file_id": "f1"})).unwrap();
        assert!(matches!(item, ResponseInputItem::Other(_)));

        let item: ResponseInputItem = serde_json::from_value(
            serde_json::json!({"type": "message", "role": "user", "content": "hi"}),
        )
        .unwrap();
        assert!(matches!(
            item,
            ResponseInputItem::Message { role, content }
                if role == "user" && content == InputMessageContent::Text("hi".to_string())
        ));

        let item: ResponseInputItem = serde_json::from_value(serde_json::json!({
            "type": "function_call",
            "call_id": "call_1",
            "name": "get_weather",
            "arguments": "{}"
        }))
        .unwrap();
        assert!(matches!(
            item,
            ResponseInputItem::FunctionCall {
                call_id,
                name,
                arguments
            } if call_id == "call_1" && name == "get_weather" && arguments == "{}"
        ));
    }

    #[test]
    fn message_content_serializes_string_or_parts() {
        let text = InputMessageContent::Text("hi".to_string());
        assert_eq!(serde_json::to_value(&text).unwrap(), "hi");

        let parts = InputMessageContent::Parts(vec![
            InputContentPart::InputText {
                text: "a".to_string(),
            },
            InputContentPart::InputImage {
                image_url: "data:image/png;base64,AAAA".to_string(),
            },
        ]);
        let json = serde_json::to_value(&parts).unwrap();
        assert_eq!(json[0]["type"], "input_text");
        assert_eq!(json[1]["type"], "input_image");
        assert_eq!(json[1]["image_url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn response_tool_uses_parameters_key() {
        let tool = ResponseTool {
            kind: "function".to_string(),
            name: "get_weather".to_string(),
            description: Some("query weather".to_string()),
            parameters: serde_json::json!({"type": "object"}),
        };
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["type"], "function");
        assert_eq!(json["name"], "get_weather");
        assert_eq!(json["parameters"], serde_json::json!({"type": "object"}));
        assert!(json.get("input_schema").is_none());
    }

    #[test]
    fn response_object_decodes_from_wire_shape() {
        let json = serde_json::json!({
            "id": "resp_1",
            "object": "response",
            "created_at": 1785508178,
            "status": "completed",
            "error": null,
            "incomplete_details": null,
            "model": "deepseek-v4-flash",
            "output": [{
                "type": "message",
                "id": "msg_1",
                "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hi"}]
            }],
            "usage": {
                "input_tokens": 268,
                "input_tokens_details": {"cached_tokens": 256},
                "output_tokens": 41,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": 309
            }
        });
        let response: ResponseObject = serde_json::from_value(json).unwrap();
        assert_eq!(response.id, "resp_1");
        assert_eq!(response.model, "deepseek-v4-flash");
        assert_eq!(response.status.as_deref(), Some("completed"));
        assert_eq!(response.output.len(), 1);
        let usage = response.usage.unwrap();
        assert_eq!(usage.input_tokens, 268);
        assert_eq!(usage.input_tokens_details.unwrap().cached_tokens, Some(256));
        assert_eq!(
            usage.output_tokens_details.unwrap().reasoning_tokens,
            Some(0)
        );
    }

    #[test]
    fn output_item_unknown_type_falls_back_to_other() {
        let item: ResponseOutputItem =
            serde_json::from_value(serde_json::json!({"type": "local_search_call", "id": "l1"}))
                .unwrap();
        assert!(matches!(item, ResponseOutputItem::Other(_)));

        let item: ResponseOutputItem = serde_json::from_value(serde_json::json!({
            "type": "message",
            "id": "msg_1",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "hi"}]
        }))
        .unwrap();
        assert!(matches!(item, ResponseOutputItem::Message { .. }));

        let item: ResponseOutputItem = serde_json::from_value(serde_json::json!({
            "type": "function_call",
            "id": "fc_1",
            "call_id": "call_1",
            "name": "get_weather",
            "arguments": "{}"
        }))
        .unwrap();
        match item {
            ResponseOutputItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                assert_eq!(call_id, "call_1");
                assert_eq!(name, "get_weather");
                assert_eq!(arguments, "{}");
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn event_dispatch_by_type_and_unknown_fallback() {
        let event: ResponseEvent = serde_json::from_value(serde_json::json!({
            "type": "response.created",
            "response": {"id": "r1", "model": "m1", "output": []},
            "sequence_number": 0
        }))
        .unwrap();
        assert!(matches!(event, ResponseEvent::ResponseCreated { .. }));

        let event: ResponseEvent = serde_json::from_value(serde_json::json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "delta": "hi",
            "sequence_number": 4
        }))
        .unwrap();
        match event {
            ResponseEvent::ResponseOutputTextDelta {
                output_index,
                delta,
            } => {
                assert_eq!(output_index, 0);
                assert_eq!(delta, "hi");
            }
            other => panic!("expected OutputTextDelta, got {other:?}"),
        }

        let event: ResponseEvent =
            serde_json::from_value(serde_json::json!({"type": "response.future_event", "x": 1}))
                .unwrap();
        assert!(matches!(event, ResponseEvent::Other(_)));
    }

    #[test]
    fn event_with_unknown_field_shape_still_decodes() {
        // 未知事件类型必须兜底，不能破坏整个 SSE 流。
        let event: ResponseEvent = serde_json::from_value(serde_json::json!({
            "type": "response.error",
            "code": "server_error",
            "message": "boom",
            "sequence_number": 42
        }))
        .unwrap();
        assert!(matches!(event, ResponseEvent::Other(_)));
    }
}

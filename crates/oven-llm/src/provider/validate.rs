//! 请求校验：`validate_request` / `estimate_input_tokens`。

use thiserror::Error;

use super::model::ModelInfo;
use crate::domain::message::ContentBlock;
use crate::domain::request::Request;
use crate::domain::tool::Tool;

/// `validate_request` 的校验失败原因。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    /// `Request.sampling.max_tokens` 超过模型的 `max_output_tokens`。
    #[error("max_tokens {requested} exceeds model's max_output_tokens {max}")]
    MaxTokensExceeded { requested: u32, max: u32 },
    /// 估算的输入 token 数超过模型的 `context_window`。
    #[error("estimated input tokens {estimated} exceed model's context_window {window}")]
    ContextOverflow { estimated: u32, window: u32 },
    /// 请求携带了 `tools`，但模型不支持工具调用。
    #[error("model does not support tools")]
    ToolsUnsupported,
    /// 请求携带了图片内容块，但模型不支持视觉输入。
    #[error("model does not support vision input")]
    VisionUnsupported,
    /// 请求要求流式响应，但模型不支持流式。
    #[error("model does not support streaming")]
    StreamingUnsupported,
    /// 请求要求 JSON 模式，但模型不支持。
    #[error("model does not support JSON mode")]
    JsonModeUnsupported,
    /// 请求携带了 `system` 提示词，但模型不支持 system prompt。
    #[error("model does not support system prompt")]
    SystemPromptUnsupported,
    /// 请求要求并行工具调用，但模型不支持。
    ///
    /// 注意：`validate_request` 目前不对 `supports_parallel_tool_calls` 做任何
    /// 静态校验（Requirement 9.8），该变体保留仅为未来可能的场景预留。
    #[error("model does not support parallel tool calls")]
    ParallelToolCallsUnsupported,
}

/// 估算一段文本的 token 数：按 ~3 字符/token 的保守估算，向上取整。
fn estimate_text_tokens(char_count: usize) -> u32 {
    (char_count as u32).div_ceil(3)
}

/// 递归统计一个 `ContentBlock` 中所有文本内容的字符数（`Text`、
/// `ToolUse.input`、`ToolResult` 内嵌 `content` 均计入；`Image` 不计入）。
fn content_block_char_count(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Thinking { thinking } => thinking.chars().count(),
        ContentBlock::Text { text } => text.chars().count(),
        ContentBlock::Image { .. } => 0,
        ContentBlock::ToolUse { name, input, .. } => {
            name.chars().count() + input.to_string().chars().count()
        }
        ContentBlock::ToolResult { content, .. } => {
            content.iter().map(content_block_char_count).sum()
        }
    }
}

/// 统计一个 `Tool` 定义（name + description + input_schema 的 JSON 字符串）
/// 的字符数。
fn tool_char_count(tool: &Tool) -> usize {
    let mut count = tool.name.chars().count();
    if let Some(description) = &tool.description {
        count += description.chars().count();
    }
    count += tool.input_schema.to_string().chars().count();
    count
}

/// 检查 `Request.messages` 中是否存在任意 `ContentBlock::Image`
/// （包括嵌套在 `ToolResult` 内的图片）。
fn contains_image(blocks: &[ContentBlock]) -> bool {
    blocks.iter().any(|block| match block {
        ContentBlock::Image { .. } => true,
        ContentBlock::ToolResult { content, .. } => contains_image(content),
        ContentBlock::Thinking { .. }
        | ContentBlock::Text { .. }
        | ContentBlock::ToolUse { .. } => false,
    })
}

/// 估算一个 `Request` 的输入 token 数：按 ~3 字符/token 的保守估算，覆盖
/// `system`、`messages` 中所有内容块（递归展开 `ToolResult`）以及 `tools`
/// 定义。
pub fn estimate_input_tokens(req: &Request) -> u32 {
    let mut char_count: usize = 0;

    if let Some(system) = &req.system {
        char_count += system.chars().count();
    }

    for message in &req.messages {
        for block in &message.content {
            char_count += content_block_char_count(block);
        }
    }

    for tool in &req.tools {
        char_count += tool_char_count(tool);
    }

    estimate_text_tokens(char_count)
}

/// 在发送请求前，依据所选 `ModelInfo` 的能力、限制和响应交付模式做静态校验。
///
/// 依次检查（按此顺序）：`max_tokens`、system prompt、tools、streaming、
/// vision、context window。全部通过返回 `Ok(())`。
///
/// 明确不做的校验：`ModelCapabilities::supports_parallel_tool_calls`
/// 取决于模型响应期实际返回多少个 `tool_calls`，是运行期行为而非请求期约束，
/// 因此本函数不对其做任何静态检查（Requirement 9.8）。
pub fn validate_request(
    req: &Request,
    model: &ModelInfo,
    stream: bool,
) -> Result<(), ValidationError> {
    if let Some(requested) = req.sampling.max_tokens
        && model.max_output_tokens > 0
        && requested > model.max_output_tokens
    {
        return Err(ValidationError::MaxTokensExceeded {
            requested,
            max: model.max_output_tokens,
        });
    }

    if req.system.is_some() && !model.capabilities.supports_system_prompt {
        return Err(ValidationError::SystemPromptUnsupported);
    }

    if !req.tools.is_empty() && !model.capabilities.supports_tools {
        return Err(ValidationError::ToolsUnsupported);
    }

    if stream && !model.capabilities.supports_streaming {
        return Err(ValidationError::StreamingUnsupported);
    }

    let has_image = req
        .messages
        .iter()
        .any(|message| contains_image(&message.content));
    if has_image && !model.capabilities.supports_vision {
        return Err(ValidationError::VisionUnsupported);
    }

    if model.context_window > 0 {
        let estimated = estimate_input_tokens(req);
        if estimated > model.context_window {
            return Err(ValidationError::ContextOverflow {
                estimated,
                window: model.context_window,
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::message::{ImageSource, Message};
    use crate::domain::request::{ModelId, SamplingParams};
    use crate::domain::tool::ToolChoice;
    use crate::provider::model::ModelCapabilities;

    /// 构造一个默认全支持能力的模型（除非测试中显式覆盖），
    /// `context_window`/`max_output_tokens` 足够大以避免无关规则触发。
    fn full_capability_model() -> ModelInfo {
        ModelInfo {
            id: "test-model".to_string(),
            provider: crate::ProviderName::Custom("test".to_string()),
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_tools: true,
                supports_streaming: true,
                supports_json_mode: true,
                supports_parallel_tool_calls: true,
                supports_system_prompt: true,
                max_concurrent_tools: None,
            },
            pricing: None,
            protocols: Vec::new(),
        }
    }

    fn base_request() -> Request {
        Request {
            model: ModelId::from("test-model"),
            messages: vec![Message::user(vec![ContentBlock::text("hi")])],
            ..Default::default()
        }
    }

    // --- max_tokens (Requirement 9.1) ---

    #[test]
    fn max_tokens_exceeded_fails() {
        let model = full_capability_model();
        let req = Request {
            sampling: SamplingParams {
                max_tokens: Some(model.max_output_tokens + 1),
                ..Default::default()
            },
            ..base_request()
        };
        assert_eq!(
            validate_request(&req, &model, false),
            Err(ValidationError::MaxTokensExceeded {
                requested: model.max_output_tokens + 1,
                max: model.max_output_tokens,
            })
        );
    }

    #[test]
    fn max_tokens_within_limit_passes() {
        let model = full_capability_model();
        let req = Request {
            sampling: SamplingParams {
                max_tokens: Some(model.max_output_tokens),
                ..Default::default()
            },
            ..base_request()
        };
        assert_eq!(validate_request(&req, &model, false), Ok(()));
    }

    #[test]
    fn max_tokens_skipped_when_model_limit_unknown() {
        let mut model = full_capability_model();
        model.max_output_tokens = 0;
        let req = Request {
            sampling: SamplingParams {
                max_tokens: Some(u32::MAX),
                ..Default::default()
            },
            ..base_request()
        };
        assert_eq!(validate_request(&req, &model, false), Ok(()));
    }

    // --- system prompt (Requirement 9.2) ---

    #[test]
    fn system_prompt_unsupported_fails() {
        let mut model = full_capability_model();
        model.capabilities.supports_system_prompt = false;
        let req = Request {
            system: Some("be helpful".to_string()),
            ..base_request()
        };
        assert_eq!(
            validate_request(&req, &model, false),
            Err(ValidationError::SystemPromptUnsupported)
        );
    }

    #[test]
    fn system_prompt_supported_passes() {
        let model = full_capability_model();
        let req = Request {
            system: Some("be helpful".to_string()),
            ..base_request()
        };
        assert_eq!(validate_request(&req, &model, false), Ok(()));
    }

    // --- tools (Requirement 9.3) ---

    #[test]
    fn tools_unsupported_fails() {
        let mut model = full_capability_model();
        model.capabilities.supports_tools = false;
        let req = Request {
            tools: vec![Tool {
                name: "get_weather".to_string(),
                description: None,
                input_schema: serde_json::json!({}),
            }],
            ..base_request()
        };
        assert_eq!(
            validate_request(&req, &model, false),
            Err(ValidationError::ToolsUnsupported)
        );
    }

    #[test]
    fn tools_supported_passes() {
        let model = full_capability_model();
        let req = Request {
            tools: vec![Tool {
                name: "get_weather".to_string(),
                description: None,
                input_schema: serde_json::json!({}),
            }],
            tool_choice: ToolChoice::Auto,
            ..base_request()
        };
        assert_eq!(validate_request(&req, &model, false), Ok(()));
    }

    // --- vision (Requirement 9.5) ---

    #[test]
    fn vision_unsupported_fails() {
        let mut model = full_capability_model();
        model.capabilities.supports_vision = false;
        let req = Request {
            messages: vec![Message::user(vec![ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://example.com/a.png".to_string(),
                },
            }])],
            ..base_request()
        };
        assert_eq!(
            validate_request(&req, &model, false),
            Err(ValidationError::VisionUnsupported)
        );
    }

    #[test]
    fn vision_unsupported_fails_for_nested_tool_result_image() {
        let mut model = full_capability_model();
        model.capabilities.supports_vision = false;
        let req = Request {
            messages: vec![Message::user(vec![ContentBlock::ToolResult {
                tool_use_id: "tool_1".to_string(),
                content: vec![ContentBlock::Image {
                    source: ImageSource::Url {
                        url: "https://example.com/a.png".to_string(),
                    },
                }],
                is_error: false,
            }])],
            ..base_request()
        };
        assert_eq!(
            validate_request(&req, &model, false),
            Err(ValidationError::VisionUnsupported)
        );
    }

    #[test]
    fn vision_supported_passes() {
        let model = full_capability_model();
        let req = Request {
            messages: vec![Message::user(vec![ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://example.com/a.png".to_string(),
                },
            }])],
            ..base_request()
        };
        assert_eq!(validate_request(&req, &model, false), Ok(()));
    }

    #[test]
    fn streaming_not_support() {
        let mut model = full_capability_model();
        model.capabilities.supports_streaming = false;
        let req = Request {
            messages: vec![Message::user(vec![ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://example.com/a.png".to_string(),
                },
            }])],
            ..base_request()
        };
        assert!(validate_request(&req, &model, true).is_err());
    }

    // --- context window (Requirement 9.6) ---

    #[test]
    fn context_overflow_fails() {
        let mut model = full_capability_model();
        model.context_window = 1;
        let req = Request {
            messages: vec![Message::user(vec![ContentBlock::text(
                "this is a much longer message than one token",
            )])],
            ..base_request()
        };
        let estimated = estimate_input_tokens(&req);
        assert!(estimated > model.context_window);
        assert_eq!(
            validate_request(&req, &model, false),
            Err(ValidationError::ContextOverflow {
                estimated,
                window: model.context_window,
            })
        );
    }

    #[test]
    fn context_within_window_passes() {
        let model = full_capability_model();
        let req = base_request();
        assert_eq!(validate_request(&req, &model, false), Ok(()));
    }

    #[test]
    fn context_window_skipped_when_unknown() {
        let mut model = full_capability_model();
        model.context_window = 0;
        let req = Request {
            messages: vec![Message::user(vec![ContentBlock::text(
                "this message would overflow any small context window",
            )])],
            ..base_request()
        };
        assert_eq!(validate_request(&req, &model, false), Ok(()));
    }

    // --- Ok(()) when everything satisfied (Requirement 9.7) ---

    #[test]
    fn fully_satisfied_request_passes() {
        let model = full_capability_model();
        let req = Request {
            system: Some("be helpful".to_string()),
            messages: vec![Message::user(vec![
                ContentBlock::text("hi"),
                ContentBlock::Image {
                    source: ImageSource::Url {
                        url: "https://example.com/a.png".to_string(),
                    },
                },
            ])],
            tools: vec![Tool {
                name: "get_weather".to_string(),
                description: Some("查询天气".to_string()),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            sampling: SamplingParams {
                max_tokens: Some(model.max_output_tokens),
                ..Default::default()
            },
            ..base_request()
        };
        assert_eq!(validate_request(&req, &model, false), Ok(()));
    }

    // --- supports_parallel_tool_calls has no effect (Requirement 9.8) ---

    #[test]
    fn parallel_tool_calls_capability_does_not_affect_result() {
        let req = base_request();

        let mut model_supported = full_capability_model();
        model_supported.capabilities.supports_parallel_tool_calls = true;

        let mut model_unsupported = full_capability_model();
        model_unsupported.capabilities.supports_parallel_tool_calls = false;

        assert_eq!(
            validate_request(&req, &model_supported, false),
            validate_request(&req, &model_unsupported, false)
        );
        assert_eq!(validate_request(&req, &model_unsupported, false), Ok(()));
    }

    // --- estimate_input_tokens ---

    #[test]
    fn estimate_input_tokens_counts_system_messages_and_tools() {
        let req = Request {
            system: Some("abc".to_string()), // 3 chars
            messages: vec![Message::user(vec![ContentBlock::text("def")])], // 3 chars
            tools: vec![Tool {
                name: "ghi".to_string(), // 3 chars
                description: None,
                input_schema: serde_json::json!({}), // "{}" -> 2 chars
            }],
            ..Default::default()
        };
        // total chars = 3 + 3 + 3 + 2 = 11, ceil(11/3) = 4
        assert_eq!(estimate_input_tokens(&req), 4);
    }

    #[test]
    fn estimate_input_tokens_empty_request_is_zero() {
        let req = Request::default();
        assert_eq!(estimate_input_tokens(&req), 0);
    }
}

//! 合并后的静态模型目录：每个模型一条，协议写在 `protocols` 里（第一个为默认）。

use super::model::{ModelCapabilities, ModelInfo, Pricing};
use crate::{ProviderKind, ProviderName};

pub fn all_models() -> Vec<ModelInfo> {
    let mut models = Vec::new();
    models.extend(deepseek_models());
    models.extend(moonshot_models());
    models.extend(zhipu_models());
    models.extend(grok_models());
    models
}

pub fn models_for(name: &ProviderName, kind: ProviderKind) -> Vec<ModelInfo> {
    all_models()
        .into_iter()
        .filter(|model| model.provider == *name && model.supports_protocol(kind))
        .collect()
}

fn deepseek_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "deepseek-v4-flash".to_string(),
            provider: ProviderName::DeepSeek,
            context_window: 1_000_000,
            max_output_tokens: 384_000,
            capabilities: ModelCapabilities {
                supports_vision: false,
                supports_tools: true,
                supports_streaming: true,
                supports_json_mode: true,
                supports_parallel_tool_calls: true,
                supports_system_prompt: true,
                max_concurrent_tools: Some(64),
            },
            pricing: Some(Pricing {
                input_per_million: 0.14,
                output_per_million: 0.28,
            }),
            protocols: vec![ProviderKind::Completions, ProviderKind::Responses],
        },
        ModelInfo {
            id: "deepseek-v4-pro".to_string(),
            provider: ProviderName::DeepSeek,
            context_window: 1_000_000,
            max_output_tokens: 384_000,
            capabilities: ModelCapabilities {
                supports_vision: false,
                supports_tools: true,
                supports_streaming: true,
                supports_json_mode: true,
                supports_parallel_tool_calls: true,
                supports_system_prompt: true,
                max_concurrent_tools: Some(128),
            },
            pricing: Some(Pricing {
                input_per_million: 2.5,
                output_per_million: 10.0,
            }),
            protocols: vec![ProviderKind::Completions],
        },
    ]
}

fn moonshot_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "kimi-k3".to_string(),
            provider: ProviderName::Moonshot,
            context_window: 1_048_576,
            max_output_tokens: 128_000,
            capabilities: ModelCapabilities {
                supports_vision: true,
                supports_tools: true,
                supports_streaming: true,
                supports_json_mode: true,
                supports_parallel_tool_calls: true,
                supports_system_prompt: true,
                max_concurrent_tools: Some(128),
            },
            pricing: Some(Pricing {
                input_per_million: 1.2,
                output_per_million: 4.8,
            }),
            protocols: vec![ProviderKind::Completions],
        },
        ModelInfo {
            id: "kimi-k2.7-code".to_string(),
            provider: ProviderName::Moonshot,
            context_window: 256_000,
            max_output_tokens: 128_000,
            capabilities: ModelCapabilities {
                supports_vision: false,
                supports_tools: true,
                supports_streaming: true,
                supports_json_mode: true,
                supports_parallel_tool_calls: true,
                supports_system_prompt: true,
                max_concurrent_tools: Some(64),
            },
            pricing: Some(Pricing {
                input_per_million: 0.6,
                output_per_million: 2.4,
            }),
            protocols: vec![ProviderKind::Completions],
        },
        ModelInfo {
            id: "kimi-k2.6".to_string(),
            provider: ProviderName::Moonshot,
            context_window: 256_000,
            max_output_tokens: 128_000,
            capabilities: ModelCapabilities {
                supports_vision: false,
                supports_tools: true,
                supports_streaming: true,
                supports_json_mode: false,
                supports_parallel_tool_calls: false,
                supports_system_prompt: true,
                max_concurrent_tools: Some(32),
            },
            pricing: Some(Pricing {
                input_per_million: 0.3,
                output_per_million: 1.2,
            }),
            protocols: vec![ProviderKind::Completions],
        },
    ]
}

fn zhipu_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "glm-5.3".to_string(),
            provider: ProviderName::Zhipu,
            context_window: 1_000_000,
            max_output_tokens: 128_000,
            capabilities: ModelCapabilities {
                supports_vision: false,
                supports_tools: true,
                supports_streaming: true,
                supports_json_mode: true,
                supports_parallel_tool_calls: true,
                supports_system_prompt: true,
                max_concurrent_tools: Some(128),
            },
            pricing: Some(Pricing {
                input_per_million: 1.0,
                output_per_million: 4.0,
            }),
            protocols: vec![ProviderKind::Completions],
        },
        ModelInfo {
            id: "glm-5.2".to_string(),
            provider: ProviderName::Zhipu,
            context_window: 1_000_000,
            max_output_tokens: 128_000,
            capabilities: ModelCapabilities {
                supports_vision: false,
                supports_tools: true,
                supports_streaming: true,
                supports_json_mode: true,
                supports_parallel_tool_calls: true,
                supports_system_prompt: true,
                max_concurrent_tools: Some(128),
            },
            pricing: Some(Pricing {
                input_per_million: 1.0,
                output_per_million: 4.0,
            }),
            protocols: vec![ProviderKind::Completions],
        },
        ModelInfo {
            id: "glm-5.1".to_string(),
            provider: ProviderName::Zhipu,
            context_window: 200_000,
            max_output_tokens: 128_000,
            capabilities: ModelCapabilities {
                supports_vision: false,
                supports_tools: true,
                supports_streaming: true,
                supports_json_mode: true,
                supports_parallel_tool_calls: true,
                supports_system_prompt: true,
                max_concurrent_tools: Some(64),
            },
            pricing: Some(Pricing {
                input_per_million: 0.5,
                output_per_million: 2.0,
            }),
            protocols: vec![ProviderKind::Completions],
        },
        ModelInfo {
            id: "glm-5".to_string(),
            provider: ProviderName::Zhipu,
            context_window: 200_000,
            max_output_tokens: 128_000,
            capabilities: ModelCapabilities {
                supports_vision: false,
                supports_tools: true,
                supports_streaming: true,
                supports_json_mode: true,
                supports_parallel_tool_calls: true,
                supports_system_prompt: true,
                max_concurrent_tools: Some(32),
            },
            pricing: Some(Pricing {
                input_per_million: 0.2,
                output_per_million: 0.8,
            }),
            protocols: vec![ProviderKind::Completions],
        },
        ModelInfo {
            id: "glm-4.7-flash".to_string(),
            provider: ProviderName::Zhipu,
            context_window: 200_000,
            max_output_tokens: 128_000,
            capabilities: ModelCapabilities {
                supports_vision: false,
                supports_tools: true,
                supports_streaming: true,
                supports_json_mode: true,
                supports_parallel_tool_calls: true,
                supports_system_prompt: true,
                max_concurrent_tools: Some(32),
            },
            pricing: Some(Pricing {
                input_per_million: 0.2,
                output_per_million: 0.8,
            }),
            protocols: vec![ProviderKind::Completions],
        },
    ]
}

fn grok_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "grok-4.6".to_string(),
            provider: ProviderName::Grok,
            context_window: 256_000,
            max_output_tokens: 100_000,
            capabilities: ModelCapabilities {
                supports_vision: false,
                supports_tools: true,
                supports_streaming: true,
                supports_json_mode: true,
                supports_parallel_tool_calls: true,
                supports_system_prompt: true,
                max_concurrent_tools: Some(64),
            },
            pricing: Some(Pricing {
                input_per_million: 0.5,
                output_per_million: 1.0,
            }),
            protocols: vec![ProviderKind::Responses],
        },
        ModelInfo {
            id: "grok-build-0.1".to_string(),
            provider: ProviderName::Grok,
            context_window: 256_000,
            max_output_tokens: 100_000,
            capabilities: ModelCapabilities {
                supports_vision: false,
                supports_tools: true,
                supports_streaming: true,
                supports_json_mode: true,
                supports_parallel_tool_calls: true,
                supports_system_prompt: true,
                max_concurrent_tools: Some(64),
            },
            pricing: Some(Pricing {
                input_per_million: 0.5,
                output_per_million: 1.0,
            }),
            protocols: vec![ProviderKind::Responses],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_unique_per_provider() {
        let models = all_models();
        let mut seen = std::collections::HashSet::new();
        for model in &models {
            assert!(
                seen.insert(format!("{}:{}", model.provider.slug(), model.id)),
                "duplicate {} / {}",
                model.provider,
                model.id
            );
        }
    }

    #[test]
    fn deepseek_flash_speaks_both_protocols() {
        let flash = all_models()
            .into_iter()
            .find(|m| m.id == "deepseek-v4-flash")
            .unwrap();
        assert_eq!(flash.default_protocol(), ProviderKind::Completions);
        assert!(flash.supports_protocol(ProviderKind::Responses));
        assert!(
            !all_models()
                .iter()
                .find(|m| m.id == "deepseek-v4-pro")
                .unwrap()
                .supports_protocol(ProviderKind::Responses)
        );
    }

    #[test]
    fn grok_defaults_to_responses() {
        let grok = all_models()
            .into_iter()
            .find(|m| m.id == "grok-4.6")
            .unwrap();
        assert_eq!(grok.default_protocol(), ProviderKind::Responses);
        assert_eq!(grok.slug(), "xai/grok-4.6");
    }
}

//! 合并后的静态模型目录：每个模型一条，协议写在 `protocols` 里（第一个为默认）。

use super::model::{ModelCapabilities, ModelInfo, Pricing};
use crate::{ProviderKind, ProviderName};

pub(crate) const OPENAI_DEFAULT_MODEL: &str = "gpt-5.6-terra";
pub(crate) const DEEPSEEK_V4_FLASH: &str = "deepseek-v4-flash";
pub(crate) const KIMI_K3: &str = "kimi-k3";
pub(crate) const GLM_53: &str = "glm-5.3";
pub(crate) const GROK_46: &str = "grok-4.6";

pub(crate) const fn default_model_id(name: &ProviderName) -> Option<&'static str> {
    match name {
        ProviderName::OpenAI => Some(OPENAI_DEFAULT_MODEL),
        ProviderName::DeepSeek => Some(DEEPSEEK_V4_FLASH),
        ProviderName::Moonshot => Some(KIMI_K3),
        ProviderName::Zhipu => Some(GLM_53),
        ProviderName::Grok => Some(GROK_46),
        ProviderName::Anthropic | ProviderName::Custom(_) => None,
    }
}

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

fn caps(vision: bool, concurrent: u32) -> ModelCapabilities {
    ModelCapabilities {
        supports_vision: vision,
        max_concurrent_tools: Some(concurrent),
        ..ModelCapabilities::supported()
    }
}

fn model(
    id: &str,
    provider: ProviderName,
    limits: (u32, u32),
    capabilities: ModelCapabilities,
    pricing: (f64, f64),
    protocols: &[ProviderKind],
) -> ModelInfo {
    let (context_window, max_output_tokens) = limits;
    let (input_per_million, output_per_million) = pricing;
    ModelInfo {
        id: id.to_string(),
        provider,
        context_window,
        max_output_tokens,
        capabilities,
        pricing: Some(Pricing {
            input_per_million,
            output_per_million,
        }),
        protocols: protocols.to_vec(),
    }
}

fn deepseek_models() -> Vec<ModelInfo> {
    vec![
        model(
            DEEPSEEK_V4_FLASH,
            ProviderName::DeepSeek,
            (1_000_000, 384_000),
            caps(false, 64),
            (0.14, 0.28),
            &[ProviderKind::Completions, ProviderKind::Responses],
        ),
        model(
            "deepseek-v4-pro",
            ProviderName::DeepSeek,
            (1_000_000, 384_000),
            caps(false, 128),
            (2.5, 10.0),
            &[ProviderKind::Completions],
        ),
    ]
}

fn moonshot_models() -> Vec<ModelInfo> {
    let mut k2_6 = caps(false, 32);
    k2_6.supports_json_mode = false;
    k2_6.supports_parallel_tool_calls = false;
    vec![
        model(
            KIMI_K3,
            ProviderName::Moonshot,
            (1_048_576, 128_000),
            caps(true, 128),
            (1.2, 4.8),
            &[ProviderKind::Completions],
        ),
        model(
            "kimi-k2.7-code",
            ProviderName::Moonshot,
            (256_000, 128_000),
            caps(false, 64),
            (0.6, 2.4),
            &[ProviderKind::Completions],
        ),
        model(
            "kimi-k2.6",
            ProviderName::Moonshot,
            (256_000, 128_000),
            k2_6,
            (0.3, 1.2),
            &[ProviderKind::Completions],
        ),
    ]
}

fn zhipu_models() -> Vec<ModelInfo> {
    vec![
        model(
            GLM_53,
            ProviderName::Zhipu,
            (1_000_000, 128_000),
            caps(false, 128),
            (1.0, 4.0),
            &[ProviderKind::Completions],
        ),
        model(
            "glm-5.2",
            ProviderName::Zhipu,
            (1_000_000, 128_000),
            caps(false, 128),
            (1.0, 4.0),
            &[ProviderKind::Completions],
        ),
        model(
            "glm-5.1",
            ProviderName::Zhipu,
            (200_000, 128_000),
            caps(false, 64),
            (0.5, 2.0),
            &[ProviderKind::Completions],
        ),
        model(
            "glm-5",
            ProviderName::Zhipu,
            (200_000, 128_000),
            caps(false, 32),
            (0.2, 0.8),
            &[ProviderKind::Completions],
        ),
        model(
            "glm-4.7-flash",
            ProviderName::Zhipu,
            (200_000, 128_000),
            caps(false, 32),
            (0.2, 0.8),
            &[ProviderKind::Completions],
        ),
    ]
}

fn grok_models() -> Vec<ModelInfo> {
    vec![
        model(
            GROK_46,
            ProviderName::Grok,
            (256_000, 100_000),
            caps(false, 64),
            (0.5, 1.0),
            &[ProviderKind::Responses],
        ),
        model(
            "grok-build-0.1",
            ProviderName::Grok,
            (256_000, 100_000),
            caps(false, 64),
            (0.5, 1.0),
            &[ProviderKind::Responses],
        ),
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
            .find(|m| m.id == DEEPSEEK_V4_FLASH)
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
        let responses = models_for(&ProviderName::DeepSeek, ProviderKind::Responses);
        assert!(responses.iter().any(|model| model.id == DEEPSEEK_V4_FLASH));
        assert!(!responses.iter().any(|model| model.id == "deepseek-v4-pro"));
    }

    #[test]
    fn grok_defaults_to_responses() {
        let grok = all_models().into_iter().find(|m| m.id == GROK_46).unwrap();
        assert_eq!(grok.default_protocol(), ProviderKind::Responses);
        assert_eq!(grok.slug(), "xai/grok-4.6");
        let responses = models_for(&ProviderName::Grok, ProviderKind::Responses);
        assert!(responses.iter().any(|model| model.id == GROK_46));
        assert!(responses.iter().any(|model| model.id == "grok-build-0.1"));
    }
}

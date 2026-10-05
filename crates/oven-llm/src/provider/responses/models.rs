//! Responses 协议下的静态模型列表（从合并目录按协议过滤）。

use crate::provider::catalog;
use crate::provider::model::ModelInfo;
use crate::{ProviderKind, ProviderName};

pub fn deepseek_models() -> Vec<ModelInfo> {
    catalog::models_for(&ProviderName::DeepSeek, ProviderKind::Responses)
}

pub fn grok_models() -> Vec<ModelInfo> {
    catalog::models_for(&ProviderName::Grok, ProviderKind::Responses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn deepseek_models_have_unique_ids_and_provider() {
        let models = deepseek_models();
        let mut seen = HashSet::new();
        for model in &models {
            assert!(seen.insert(model.id.clone()));
            assert_eq!(model.provider, ProviderName::DeepSeek);
            assert!(model.supports_protocol(ProviderKind::Responses));
        }
        assert!(models.iter().any(|m| m.id == "deepseek-v4-flash"));
        assert!(!models.iter().any(|m| m.id == "deepseek-v4-pro"));
    }

    #[test]
    fn grok_models_have_unique_ids_and_provider() {
        let models = grok_models();
        let mut seen = HashSet::new();
        for model in &models {
            assert!(seen.insert(model.id.clone()));
            assert_eq!(model.provider, ProviderName::Grok);
        }
        assert!(models.iter().any(|m| m.id == "grok-4.6"));
        assert!(models.iter().any(|m| m.id == "grok-build-0.1"));
    }
}

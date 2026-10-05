//! Completions 协议下的静态模型列表（从合并目录按协议过滤）。

use crate::provider::catalog;
use crate::provider::model::ModelInfo;
use crate::{ProviderKind, ProviderName};

pub fn deepseek_models() -> Vec<ModelInfo> {
    catalog::models_for(&ProviderName::DeepSeek, ProviderKind::Completions)
}

pub fn moonshot_models() -> Vec<ModelInfo> {
    catalog::models_for(&ProviderName::Moonshot, ProviderKind::Completions)
}

pub fn zhipu_models() -> Vec<ModelInfo> {
    catalog::models_for(&ProviderName::Zhipu, ProviderKind::Completions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn assert_unique_ids(models: &[ModelInfo]) {
        let mut seen = HashSet::new();
        for model in models {
            assert!(
                seen.insert(model.id.clone()),
                "duplicate model id: {}",
                model.id
            );
        }
    }

    fn assert_provider(models: &[ModelInfo], expected: &ProviderName) {
        for model in models {
            assert_eq!(
                model.provider, *expected,
                "model {} has unexpected provider {:?}",
                model.id, model.provider
            );
            assert!(model.supports_protocol(ProviderKind::Completions));
        }
    }

    #[test]
    fn deepseek_models_have_unique_ids_and_provider() {
        let models = deepseek_models();
        assert_unique_ids(&models);
        assert_provider(&models, &ProviderName::DeepSeek);
    }

    #[test]
    fn moonshot_models_have_unique_ids_and_provider() {
        let models = moonshot_models();
        assert_unique_ids(&models);
        assert_provider(&models, &ProviderName::Moonshot);
    }

    #[test]
    fn zhipu_models_have_unique_ids_and_provider() {
        let models = zhipu_models();
        assert_unique_ids(&models);
        assert_provider(&models, &ProviderName::Zhipu);
        assert!(models.iter().any(|m| m.id == "glm-5.3"));
    }
}

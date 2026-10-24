//! profile：跨厂商的质量预设。
//!
//! profile 是我们定的名字，落到每个厂商各自的模型与强度上：同一个
//! `balanced` 在 deepseek 是一种模型，在 moonshot 是另一种。请求路径不查
//! 表——查表只发生在切换 profile、切换厂商与启动这三处，落到配置里之后，
//! 请求路径看到的就只是"模型 + 强度"。

use oven_llm::ProviderName;

use super::config::{AppConfig, ProfileBinding, ProfileTable};

/// profile 在某个厂商下的落法。没有这条绑定就是没有。
pub fn binding<'a>(
    profiles: &'a ProfileTable,
    name: &str,
    provider: &ProviderName,
) -> Option<&'a ProfileBinding> {
    profiles.get(name)?.get(provider.slug())
}

/// 该厂商下可用的 profile 名，按名字排。
pub fn available<'a>(profiles: &'a ProfileTable, provider: &ProviderName) -> Vec<&'a str> {
    let mut names: Vec<&str> = profiles
        .iter()
        .filter(|(_, vendors)| vendors.contains_key(provider.slug()))
        .map(|(name, _)| name.as_str())
        .collect();
    names.sort_unstable();
    names
}

/// 当前厂商：`active` 的名字。
pub fn active_provider(config: &AppConfig) -> ProviderName {
    ProviderName::from(config.active_provider.name.as_str())
}

/// 该厂商下这个 profile 会落到哪个模型（带厂商前缀）。
pub fn qualified_model(binding: &ProfileBinding, provider: &ProviderName) -> String {
    oven_llm::ModelId::from(binding.model.as_str())
        .qualify(provider.slug())
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::core::config::ProfileBinding;

    fn table() -> ProfileTable {
        let binding = |model: &str| ProfileBinding {
            model: model.to_string(),
            effort: None,
        };
        let mut balanced: BTreeMap<String, ProfileBinding> = BTreeMap::new();
        balanced.insert("deepseek".to_string(), binding("deepseek-v4-flash"));
        balanced.insert("moonshot".to_string(), binding("kimi-k3"));
        let mut profiles: ProfileTable = BTreeMap::new();
        profiles.insert("balanced".to_string(), balanced);
        profiles
    }

    #[test]
    fn a_binding_is_looked_up_by_vendor() {
        let profiles = table();
        let deepseek = ProviderName::DeepSeek;

        assert_eq!(
            binding(&profiles, "balanced", &deepseek).map(|b| b.model.as_str()),
            Some("deepseek-v4-flash")
        );
        assert!(binding(&profiles, "deep", &deepseek).is_none());
    }

    #[test]
    fn a_vendor_without_the_profile_has_no_binding() {
        let profiles = table();
        let zhipu = ProviderName::Zhipu;

        assert!(binding(&profiles, "balanced", &zhipu).is_none());
        assert!(available(&profiles, &zhipu).is_empty());
    }

    #[test]
    fn the_available_names_are_listed_per_vendor() {
        let profiles = table();

        assert_eq!(
            available(&profiles, &ProviderName::DeepSeek),
            vec!["balanced"]
        );
        assert_eq!(
            available(&profiles, &ProviderName::Moonshot),
            vec!["balanced"]
        );
    }

    #[test]
    fn the_model_is_qualified_with_its_vendor() {
        let profiles = table();
        let binding = binding(&profiles, "balanced", &ProviderName::Moonshot).unwrap();

        assert_eq!(
            qualified_model(binding, &ProviderName::Moonshot),
            "moonshot/kimi-k3"
        );
    }
}

//! 厂商画像：推理方言与各厂商的能力默认值。
//!
//! 厂商知识跟着厂商走：这里是"谁家的模型支持哪些档位、wire 上用哪条通道
//! 说话"的唯一出处，模型目录只登记例外。

pub mod capability;
mod deepseek;
mod glm;
mod moonshot;
mod openai;
mod resolve;
mod xai;

pub use capability::EffortSupport;
pub use resolve::{Channels, channels, resolve};

use crate::ProviderKind;
use crate::ProviderName;

/// 推理方言：wire 上用哪条通道说话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// 档位通道：`reasoning_effort`（Completions）/ `reasoning.effort`（Responses）。
    Level,
    /// 开关通道：`thinking.type`（GLM 类，只有开/关）。
    Toggle,
    /// 两条都发：自定义服务与代理的兜底，让上游各取所需。
    Both,
}

/// 一个厂商在某个协议下的画像。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorProfile {
    /// wire 协议。
    pub protocol: ProviderKind,
    /// 推理方言。
    pub dialect: Dialect,
}

impl VendorProfile {
    /// 该厂商在给定协议下的画像。协议由调用方选定，方言由厂商决定。
    pub fn for_protocol(provider: &ProviderName, protocol: ProviderKind) -> Self {
        Self {
            protocol,
            dialect: dialect_for(provider),
        }
    }
}

/// 该厂商的推理方言。
pub fn dialect_for(provider: &ProviderName) -> Dialect {
    match provider {
        ProviderName::OpenAI => openai::DIALECT,
        ProviderName::DeepSeek => deepseek::DIALECT,
        ProviderName::Moonshot => moonshot::DIALECT,
        ProviderName::Zhipu => glm::DIALECT,
        ProviderName::Grok => xai::DIALECT,
        ProviderName::Anthropic | ProviderName::Custom(_) => Dialect::Both,
    }
}

/// 该厂商的模型默认支持哪些档位。
pub fn capabilities_for(provider: &ProviderName) -> EffortSupport {
    match provider {
        ProviderName::OpenAI => openai::CAPABILITIES,
        ProviderName::DeepSeek => deepseek::CAPABILITIES,
        ProviderName::Moonshot => moonshot::CAPABILITIES,
        ProviderName::Zhipu => glm::CAPABILITIES,
        ProviderName::Grok => xai::CAPABILITIES,
        ProviderName::Anthropic | ProviderName::Custom(_) => EffortSupport::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zhipu_speaks_the_thinking_switch() {
        assert_eq!(dialect_for(&ProviderName::Zhipu), Dialect::Toggle);
    }

    #[test]
    fn openai_family_speaks_the_level_channel() {
        assert_eq!(dialect_for(&ProviderName::OpenAI), Dialect::Level);
        assert_eq!(dialect_for(&ProviderName::DeepSeek), Dialect::Level);
        assert_eq!(dialect_for(&ProviderName::Grok), Dialect::Level);
    }

    #[test]
    fn an_unverified_vendor_keeps_sending_both_channels() {
        assert_eq!(dialect_for(&ProviderName::Moonshot), Dialect::Both);
    }

    #[test]
    fn custom_services_send_both_channels() {
        assert_eq!(
            dialect_for(&ProviderName::Custom("proxy".into())),
            Dialect::Both
        );
    }

    #[test]
    fn a_profile_carries_the_protocol_it_was_built_for() {
        let profile = VendorProfile::for_protocol(&ProviderName::Zhipu, ProviderKind::Completions);
        assert_eq!(profile.protocol, ProviderKind::Completions);
        assert_eq!(profile.dialect, Dialect::Toggle);
    }
}

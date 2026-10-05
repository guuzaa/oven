//! `ModelInfo` / `ModelCapabilities` / `Pricing`：模型能力身份证。

use crate::{ProviderKind, ProviderName};

/// 描述单个模型的元数据（所属 provider、上下文窗口、最大输出 token、能力集合、定价）。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    /// 上游 wire id，不含 vendor 前缀。
    pub id: String,
    pub provider: ProviderName,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub capabilities: ModelCapabilities,
    pub pricing: Option<Pricing>,
    /// 该模型会说的协议；第一个是默认。空表示只走 Completions。
    pub protocols: Vec<ProviderKind>,
}

impl ModelCapabilities {
    /// 调用方没写明的能力：按支持处理。`minimal` 仍是全关，留给目录里
    /// 能力未知的条目。
    pub const fn supported() -> Self {
        Self {
            supports_vision: true,
            supports_tools: true,
            supports_streaming: true,
            supports_json_mode: true,
            supports_parallel_tool_calls: true,
            supports_system_prompt: true,
            max_concurrent_tools: None,
        }
    }
}

impl ModelInfo {
    /// 用户声明的模型：窗口和输出上限保持 0（未知），能力默认支持。
    pub fn declared(id: impl Into<String>, provider: ProviderName) -> Self {
        Self {
            id: id.into(),
            provider,
            context_window: 0,
            max_output_tokens: 0,
            capabilities: ModelCapabilities::supported(),
            pricing: None,
            protocols: Vec::new(),
        }
    }

    /// 构造一个仅包含 `id`/`provider` 的最小 `ModelInfo`，其余字段填充为
    /// 零值/默认值。用于 `Provider::list_models` 等无法获得完整能力信息的场景
    /// （Requirement 8.1）。
    pub fn minimal(id: impl Into<String>, provider: ProviderName) -> Self {
        Self {
            id: id.into(),
            provider,
            context_window: 0,
            max_output_tokens: 0,
            capabilities: ModelCapabilities::default(),
            pricing: None,
            protocols: Vec::new(),
        }
    }

    pub fn default_protocol(&self) -> ProviderKind {
        self.protocols
            .first()
            .copied()
            .unwrap_or(ProviderKind::Completions)
    }

    pub fn supports_protocol(&self, kind: ProviderKind) -> bool {
        if self.protocols.is_empty() {
            kind == ProviderKind::Completions
        } else {
            self.protocols.contains(&kind)
        }
    }

    /// `vendor/wire-id`，vendor 用 [`ProviderName::slug`]。
    pub fn slug(&self) -> String {
        if self.id.contains('/') {
            self.id.clone()
        } else {
            format!("{}/{}", self.provider.slug(), self.id)
        }
    }
}

/// `ModelInfo` 中描述模型支持的功能集合。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ModelCapabilities {
    pub supports_vision: bool,
    pub supports_tools: bool,
    pub supports_streaming: bool,
    pub supports_json_mode: bool,
    pub supports_parallel_tool_calls: bool,
    pub supports_system_prompt: bool,
    pub max_concurrent_tools: Option<u32>,
}

/// 模型的定价信息（每百万 token 的价格）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderKind;

    #[test]
    fn minimal_fills_zero_defaults() {
        let info = ModelInfo::minimal("gpt-4", ProviderName::OpenAI);
        assert_eq!(info.id, "gpt-4");
        assert_eq!(info.provider, ProviderName::OpenAI);
        assert_eq!(info.context_window, 0);
        assert_eq!(info.max_output_tokens, 0);
        assert_eq!(info.capabilities, ModelCapabilities::default());
        assert_eq!(info.pricing, None);
        assert!(info.protocols.is_empty());
        assert_eq!(info.default_protocol(), ProviderKind::Completions);
        assert_eq!(info.slug(), "openai/gpt-4");
        assert!(!info.capabilities.supports_tools);
    }

    #[test]
    fn declared_treats_unset_capabilities_as_supported() {
        let info = ModelInfo::declared("local", ProviderName::Custom("gw".into()));
        assert_eq!(info.context_window, 0);
        assert_eq!(info.max_output_tokens, 0);
        assert!(info.capabilities.supports_vision);
        assert!(info.capabilities.supports_tools);
        assert!(info.capabilities.supports_streaming);
        assert!(info.capabilities.supports_json_mode);
        assert!(info.capabilities.supports_parallel_tool_calls);
        assert!(info.capabilities.supports_system_prompt);
        assert_eq!(info.capabilities.max_concurrent_tools, None);
    }
}

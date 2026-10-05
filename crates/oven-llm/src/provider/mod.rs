//! `provider` 层：`Provider` trait、错误类型与模型能力信息。

mod builder;
pub(crate) mod catalog;
mod completions;
mod error;
pub(crate) mod http;
pub mod model;
mod responses;
mod retry;
mod router;
pub mod validate;

pub use builder::ProviderBuilder;
pub use completions::CompletionsProvider;
pub use error::{ProviderError, Result};
pub use model::{ModelCapabilities, ModelInfo, Pricing};
pub use responses::ResponsesProvider;
pub use router::{Router, RouterError, RouterHandle};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
pub use validate::{ValidationError, estimate_input_tokens, validate_request};

use async_trait::async_trait;
use futures::stream::BoxStream;
use std::fmt;

use crate::domain::{ModelId, Request, Response, StreamEvent};

const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
const MOONSHOT_BASE_URL: &str = "https://api.moonshot.cn/v1";
const ZHIPU_BASE_URL: &str = "https://open.bigmodel.cn/api/paas/v4";
const GROK_BASE_URL: &str = "https://api.x.ai/v1";

/// 统一的 LLM 调用抽象：harness/应用代码只依赖该 trait 与 domain 类型，
/// 永远不接触任何 wire format 类型。
#[async_trait]
pub trait Provider: Send + Sync {
    /// 发送一次非流式请求，返回完整响应。
    async fn complete(&self, req: &Request) -> Result<Response>;

    /// 发送一次流式请求，返回统一的 `StreamEvent` 流。
    async fn stream(&self, req: &Request) -> Result<BoxStream<'static, Result<StreamEvent>>>;

    /// 该 provider 静态已知的模型列表，默认空，由具体实现覆写。
    fn known_models(&self) -> Vec<ModelInfo> {
        vec![]
    }

    /// 查找静态模型元数据。未命中表示 provider 将以宽松策略透传该模型请求。
    fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo>;

    /// 该实现对应的 wire 协议。Router 按 slug 选协议时使用；默认未知。
    fn protocol(&self) -> Option<ProviderKind> {
        None
    }

    /// 从 provider 的 `/models` 端点动态获取模型列表（能力字段通常不可用）。
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![])
    }

    fn provider_name(&self) -> ProviderName;
}

/// A boxed concrete provider registers without the caller unsizing it first.
#[async_trait]
impl<P> Provider for Box<P>
where
    P: Provider + Sized + 'static,
{
    async fn complete(&self, req: &Request) -> Result<Response> {
        Provider::complete(&**self, req).await
    }

    async fn stream(&self, req: &Request) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        Provider::stream(&**self, req).await
    }

    fn known_models(&self) -> Vec<ModelInfo> {
        Provider::known_models(&**self)
    }

    fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
        Provider::resolve_model(&**self, id)
    }

    fn protocol(&self) -> Option<ProviderKind> {
        Provider::protocol(&**self)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Provider::list_models(&**self).await
    }

    fn provider_name(&self) -> ProviderName {
        Provider::provider_name(&**self)
    }
}

/// `Box<dyn Provider>` is itself a provider, so [`Router::register`] can take
/// either a concrete client or one already boxed by [`ProviderBuilder::build`].
#[async_trait]
impl Provider for Box<dyn Provider> {
    async fn complete(&self, req: &Request) -> Result<Response> {
        Provider::complete(&**self, req).await
    }

    async fn stream(&self, req: &Request) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        Provider::stream(&**self, req).await
    }

    fn known_models(&self) -> Vec<ModelInfo> {
        Provider::known_models(&**self)
    }

    fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
        Provider::resolve_model(&**self, id)
    }

    fn protocol(&self) -> Option<ProviderKind> {
        Provider::protocol(&**self)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Provider::list_models(&**self).await
    }

    fn provider_name(&self) -> ProviderName {
        Provider::provider_name(&**self)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderName {
    OpenAI,
    DeepSeek,
    Moonshot,
    Zhipu,
    Anthropic,
    Grok,
    Custom(String),
}

impl ProviderName {
    /// slug 用的规范 vendor 名（`xai` 而不是 `grok`）。
    pub fn slug(&self) -> &str {
        match self {
            ProviderName::OpenAI => "openai",
            ProviderName::DeepSeek => "deepseek",
            ProviderName::Moonshot => "moonshot",
            ProviderName::Zhipu => "zhipu",
            ProviderName::Anthropic => "anthropic",
            ProviderName::Grok => "xai",
            ProviderName::Custom(name) => name.as_str(),
        }
    }

    pub fn matches_vendor(&self, vendor: &str) -> bool {
        crate::canonical_vendor(vendor) == self.slug()
    }

    pub fn base_url(&self) -> Option<&'static str> {
        match self {
            ProviderName::OpenAI => Some(OPENAI_BASE_URL),
            ProviderName::DeepSeek => Some(DEEPSEEK_BASE_URL),
            ProviderName::Moonshot => Some(MOONSHOT_BASE_URL),
            ProviderName::Zhipu => Some(ZHIPU_BASE_URL),
            ProviderName::Grok => Some(GROK_BASE_URL),
            _ => None,
        }
    }

    pub fn default_protocol(&self) -> Option<ProviderKind> {
        match self {
            ProviderName::DeepSeek
            | ProviderName::Moonshot
            | ProviderName::Zhipu
            | ProviderName::Custom(_) => Some(ProviderKind::Completions),
            ProviderName::OpenAI | ProviderName::Grok => Some(ProviderKind::Responses),
            ProviderName::Anthropic => None,
        }
    }

    /// 该厂商建议的默认模型 wire id。没有预设时返回 `None`。
    pub fn default_model(&self) -> Option<&'static str> {
        match self {
            ProviderName::OpenAI => Some("gpt-5.6-terra"),
            ProviderName::DeepSeek => Some("deepseek-v4-flash"),
            ProviderName::Moonshot => Some("kimi-k3"),
            ProviderName::Zhipu => Some("glm-5.3"),
            ProviderName::Grok => Some("grok-4.6"),
            ProviderName::Anthropic | ProviderName::Custom(_) => None,
        }
    }

    /// 默认协议下的静态目录。不构造客户端，也不需要 API key。
    pub fn known_models(&self) -> Vec<ModelInfo> {
        self.default_protocol()
            .map(|kind| catalog::models_for(self, kind))
            .unwrap_or_default()
    }
}

impl From<&str> for ProviderName {
    fn from(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "openai" => ProviderName::OpenAI,
            "deepseek" => ProviderName::DeepSeek,
            "moonshot" | "kimi" => ProviderName::Moonshot,
            "zhipu" | "glm" => ProviderName::Zhipu,
            "anthropic" => ProviderName::Anthropic,
            "grok" | "xai" => ProviderName::Grok,
            other => ProviderName::Custom(other.to_string()),
        }
    }
}

impl Serialize for ProviderName {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            ProviderName::OpenAI => serializer.serialize_str("openai"),
            ProviderName::DeepSeek => serializer.serialize_str("deepseek"),
            ProviderName::Moonshot => serializer.serialize_str("moonshot"),
            ProviderName::Zhipu => serializer.serialize_str("zhipu"),
            ProviderName::Anthropic => serializer.serialize_str("anthropic"),
            ProviderName::Grok => serializer.serialize_str("xai"),
            ProviderName::Custom(name) => {
                serializer.serialize_str(&format!("custom({})", name.to_lowercase()))
            }
        }
    }
}

impl<'de> Deserialize<'de> for ProviderName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?.to_lowercase();
        Ok(match raw.as_str() {
            "openai" => ProviderName::OpenAI,
            "deepseek" => ProviderName::DeepSeek,
            "moonshot" => ProviderName::Moonshot,
            "zhipu" => ProviderName::Zhipu,
            "anthropic" => ProviderName::Anthropic,
            "grok" | "xai" => ProviderName::Grok,
            _ => match raw
                .strip_prefix("custom(")
                .and_then(|s| s.strip_suffix(')'))
            {
                Some(name) => ProviderName::Custom(name.to_string()),
                None => ProviderName::Custom(raw),
            },
        })
    }
}

impl fmt::Display for ProviderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderName::OpenAI => f.write_str("OpenAI"),
            ProviderName::DeepSeek => f.write_str("DeepSeek"),
            ProviderName::Moonshot => f.write_str("Moonshot"),
            ProviderName::Zhipu => f.write_str("Zhipu"),
            ProviderName::Anthropic => f.write_str("Anthropic"),
            ProviderName::Grok => f.write_str("Grok"),
            ProviderName::Custom(name) => write!(f, "Custom({})", name),
        }
    }
}

/// Provider 的协议种类，用于统一构造入口（[`ProviderBuilder`]）的派发。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// OpenAI Chat Completions 兼容协议（`CompletionsProvider`）。
    Completions,
    /// OpenAI Responses API（`ResponsesProvider`）。
    Responses,
    /// Anthropic Messages API (Not Implemented yet)
    Messages,
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderKind::Completions => f.write_str("completions"),
            ProviderKind::Responses => f.write_str("responses"),
            ProviderKind::Messages => f.write_str("messages"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个不覆写 `known_models` / `list_models` 的最小 `Provider` 实现，
    /// 用于验证默认实现的行为（Requirements 2.3, 2.4, 2.5）。
    struct StubProvider;

    #[async_trait]
    impl Provider for StubProvider {
        async fn complete(&self, _req: &Request) -> Result<Response> {
            unimplemented!("not needed for this test")
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        async fn stream(&self, _req: &Request) -> Result<BoxStream<'static, Result<StreamEvent>>> {
            unimplemented!("not needed for this test")
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("Stub".into())
        }
    }

    #[test]
    fn known_models_default_is_empty() {
        let provider = StubProvider;
        assert!(provider.known_models().is_empty());
        assert!(provider.resolve_model(&ModelId::from("unknown")).is_none());
    }

    #[tokio::test]
    async fn list_models_default_is_empty() {
        let provider = StubProvider;
        let models = provider.list_models().await.unwrap();
        assert!(models.is_empty());
    }

    #[test]
    fn provider_name_serde_roundtrip() {
        let names = [
            ProviderName::OpenAI,
            ProviderName::DeepSeek,
            ProviderName::Moonshot,
            ProviderName::Zhipu,
            ProviderName::Anthropic,
            ProviderName::Grok,
            ProviderName::Custom("my-provider".into()),
        ];

        for name in names {
            let json = serde_json::to_string(&name).unwrap();
            let decoded: ProviderName = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, name);
        }

        // 已知名字输出小写字符串；`Custom` 输出 `custom(<name>)` 字符串。
        assert_eq!(
            serde_json::to_string(&ProviderName::OpenAI).unwrap(),
            r#""openai""#
        );
        assert_eq!(
            serde_json::to_string(&ProviderName::Custom("Stub".into())).unwrap(),
            r#""custom(stub)""#
        );
        assert_eq!(
            serde_json::from_str::<ProviderName>(r#""custom(my-provider)""#).unwrap(),
            ProviderName::Custom("my-provider".into())
        );
        assert_eq!(
            serde_json::from_str::<ProviderName>(r#""deepseek""#).unwrap(),
            ProviderName::DeepSeek
        );
        assert_eq!(
            serde_json::from_str::<ProviderName>(r#""OpenAI""#).unwrap(),
            ProviderName::OpenAI
        );
        assert_eq!(
            serde_json::from_str::<ProviderName>(r#""DeepSeek""#).unwrap(),
            ProviderName::DeepSeek
        );
        assert_eq!(
            serde_json::from_str::<ProviderName>(r#""CUSTOM(My-Provider)""#).unwrap(),
            ProviderName::Custom("my-provider".into())
        );
        // 未识别的裸字符串兜底为 `Custom`。
        assert_eq!(
            serde_json::from_str::<ProviderName>(r#""some-provider""#).unwrap(),
            ProviderName::Custom("some-provider".into())
        );
    }

    #[test]
    fn each_vendor_has_one_preset_protocol() {
        assert_eq!(
            ProviderName::OpenAI.default_protocol(),
            Some(ProviderKind::Responses)
        );
        assert_eq!(
            ProviderName::DeepSeek.default_protocol(),
            Some(ProviderKind::Completions)
        );
        assert_eq!(
            ProviderName::Moonshot.default_protocol(),
            Some(ProviderKind::Completions)
        );
        assert_eq!(
            ProviderName::Zhipu.default_protocol(),
            Some(ProviderKind::Completions)
        );
        assert_eq!(
            ProviderName::Grok.default_protocol(),
            Some(ProviderKind::Responses)
        );
        assert_eq!(
            ProviderName::Custom("gw".into()).default_protocol(),
            Some(ProviderKind::Completions)
        );
        assert!(ProviderName::Anthropic.default_protocol().is_none());
    }

    #[test]
    fn provider_name_display() {
        assert_eq!(ProviderName::OpenAI.to_string(), "OpenAI");
        assert_eq!(ProviderName::DeepSeek.to_string(), "DeepSeek");
        assert_eq!(ProviderName::Moonshot.to_string(), "Moonshot");
        assert_eq!(ProviderName::Zhipu.to_string(), "Zhipu");
        assert_eq!(ProviderName::Anthropic.to_string(), "Anthropic");
        assert_eq!(ProviderName::Grok.to_string(), "Grok");
        assert_eq!(
            ProviderName::Custom("My-Provider".into()).to_string(),
            "Custom(My-Provider)"
        );
    }

    #[test]
    fn provider_name_from_str() {
        let cases = [
            ("OpenAI", ProviderName::OpenAI),
            ("Openai", ProviderName::OpenAI),
            ("DeepSeek", ProviderName::DeepSeek),
            ("Grok", ProviderName::Grok),
            ("xai", ProviderName::Grok),
            ("moonshot", ProviderName::Moonshot),
            ("kimi", ProviderName::Moonshot),
            ("Anthropic", ProviderName::Anthropic),
            ("zhipu", ProviderName::Zhipu),
            ("glm", ProviderName::Zhipu),
            ("my", ProviderName::Custom("my".into())),
        ];

        for (s, expected) in cases {
            assert_eq!(ProviderName::from(s), expected);
        }
    }

    #[test]
    fn provider_kind_serde_roundtrip() {
        let kinds = [
            ProviderKind::Completions,
            ProviderKind::Responses,
            ProviderKind::Messages,
        ];

        for kind in kinds {
            let json = serde_json::to_string(&kind).unwrap();
            let decoded: ProviderKind = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, kind);
        }

        assert_eq!(
            serde_json::to_string(&ProviderKind::Responses).unwrap(),
            r#""responses""#
        );
        assert_eq!(
            serde_json::from_str::<ProviderKind>(r#""completions""#).unwrap(),
            ProviderKind::Completions
        );
    }

    #[test]
    fn provider_kind_into_slug() {
        assert_eq!(ProviderKind::Responses.to_string(), "responses");
        assert_eq!(ProviderKind::Messages.to_string(), "messages");
        assert_eq!(ProviderKind::Completions.to_string(), "completions");
    }

    #[test]
    fn default_model_follows_the_preset_table() {
        assert_eq!(ProviderName::OpenAI.default_model(), Some("gpt-5.6-terra"));
        assert_eq!(
            ProviderName::DeepSeek.default_model(),
            Some("deepseek-v4-flash")
        );
        assert_eq!(ProviderName::Moonshot.default_model(), Some("kimi-k3"));
        assert_eq!(ProviderName::Zhipu.default_model(), Some("glm-5.3"));
        assert_eq!(ProviderName::Grok.default_model(), Some("grok-4.6"));
        assert_eq!(ProviderName::Anthropic.default_model(), None);
        assert_eq!(ProviderName::Custom("gw".into()).default_model(), None);
    }

    #[test]
    fn known_models_reads_the_static_catalog_without_a_client() {
        let ids: Vec<_> = ProviderName::DeepSeek
            .known_models()
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert!(ids.iter().any(|id| id == "deepseek-v4-flash"), "{ids:?}");
        assert!(ProviderName::Custom("gw".into()).known_models().is_empty());
        assert!(ProviderName::Anthropic.known_models().is_empty());
    }
}

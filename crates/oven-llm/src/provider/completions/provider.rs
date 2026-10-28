//! `OpenAICompatProvider`：OpenAI Chat Completions 兼容协议的 `Provider`
//! 实现（传输层）。
//!
//! 本模块负责把 domain 层的 `Request`/`Response`/`StreamEvent` 与
//! `encoder`/`decoder` 串联起来，完成实际的 HTTP 调用：
//! - `build_body`：`encoder::encode_request` + `provider_options` 合并
//! - `post_chat_completions`：POST `chat/completions`，把非 2xx 状态码映射为
//!   具体的 `ProviderError` 变体
//! - `Provider::complete` / `Provider::stream`：分别对接非流式响应解码与
//!   基于 `eventsource-stream` 的 SSE 流式解码
//! - `Provider::known_models` / `Provider::list_models`：模型元数据查询

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use futures::stream::BoxStream;
use reqwest::header::HeaderMap;
use secrecy::SecretString;

use super::decoder::{self, StreamDecoder};
use super::encoder;
use super::types::{ChatCompletionChunk, ChatCompletionResponse};
use crate::ProviderName;
use crate::domain::{CallOptions, ModelId, Request, Response, StreamEvent};
use crate::provider::catalog;
use crate::provider::endpoint::Endpoint;
use crate::provider::model::ModelInfo;
use crate::provider::{
    DEEPSEEK_BASE_URL, MINIMAX_BASE_URL, MOONSHOT_BASE_URL, OPENAI_BASE_URL, Provider,
    ProviderError, ProviderKind, ZHIPU_BASE_URL,
};

/// OpenAI Chat Completions 兼容协议的通用 `Provider` 实现。
///
/// 通过 `base_url` 区分具体服务商（DeepSeek / Moonshot / 智谱 / OpenAI 官方
/// 等）。
pub struct CompletionsProvider {
    endpoint: Endpoint,
}

impl CompletionsProvider {
    /// 通过 `ProviderName` 派发构造。已知的 OpenAI 兼容服务商会自动填入对应的
    /// `base_url` 与模型元数据；对 `Custom` 或尚不支持的服务商，请改用
    /// [`with_base_url`] 或 [`with_models`]。
    ///
    /// 没有 Completions 预设的服务商（`Anthropic` / `Grok` / `Custom`）返回
    /// [`ProviderError::UnsupportedProvider`] 而不是 panic：调用方按
    /// `ProviderName` 批量建厂时必须能逐个处理失败。
    pub fn new(
        provider_name: ProviderName,
        api_key: impl Into<SecretString>,
    ) -> Result<Self, ProviderError> {
        match &provider_name {
            ProviderName::OpenAI => Ok(Self::openai(api_key)),
            ProviderName::DeepSeek => Ok(Self::deepseek(api_key)),
            ProviderName::Moonshot => Ok(Self::moonshot(api_key)),
            ProviderName::Zhipu => Ok(Self::zhipu(api_key)),
            ProviderName::MiniMax => Ok(Self::minimax(api_key)),
            ProviderName::Anthropic | ProviderName::Grok | ProviderName::Custom(_) => {
                Err(ProviderError::UnsupportedProvider {
                    kind: ProviderKind::Completions,
                    name: provider_name,
                })
            }
        }
    }

    /// 构造一个不带静态模型元数据、不带额外请求头的 provider（显式指定
    /// `base_url`，可用于 `Custom` 服务商）。
    pub fn with_base_url(
        base_url: impl Into<String>,
        provider_name: ProviderName,
        api_key: impl Into<SecretString>,
    ) -> Self {
        Self::with_models(
            base_url,
            provider_name,
            api_key,
            Vec::new(),
            HeaderMap::new(),
        )
    }

    /// 构造一个带静态模型元数据与额外请求头的 provider。
    ///
    /// `api_key` 接受 `SecretString`，也接受任何能转换为它的类型（例如
    /// `String` 或 `&str`）——内部始终以 `SecretString` 存储，drop 时零化。
    pub fn with_models(
        base_url: impl Into<String>,
        provider_name: ProviderName,
        api_key: impl Into<SecretString>,
        known_models: Vec<ModelInfo>,
        extra_headers: HeaderMap,
    ) -> Self {
        Self {
            endpoint: Endpoint::new(
                base_url,
                provider_name,
                api_key,
                known_models,
                extra_headers,
            ),
        }
    }

    /// DeepSeek 预设：`base_url = https://api.deepseek.com`。
    pub fn deepseek(api_key: impl Into<SecretString>) -> Self {
        Self::with_models(
            DEEPSEEK_BASE_URL,
            ProviderName::DeepSeek,
            api_key,
            catalog::models_for(&ProviderName::DeepSeek, ProviderKind::Completions),
            HeaderMap::new(),
        )
    }

    /// Moonshot（Kimi）预设：`base_url = https://api.moonshot.cn/v1`。
    pub fn moonshot(api_key: impl Into<SecretString>) -> Self {
        Self::with_models(
            MOONSHOT_BASE_URL,
            ProviderName::Moonshot,
            api_key,
            catalog::models_for(&ProviderName::Moonshot, ProviderKind::Completions),
            HeaderMap::new(),
        )
    }

    /// 智谱 GLM 预设：`base_url = https://open.bigmodel.cn/api/paas/v4`。
    pub fn zhipu(api_key: impl Into<SecretString>) -> Self {
        Self::with_models(
            ZHIPU_BASE_URL,
            ProviderName::Zhipu,
            api_key,
            catalog::models_for(&ProviderName::Zhipu, ProviderKind::Completions),
            HeaderMap::new(),
        )
    }

    /// MiniMax 预设：`base_url = https://api.minimax.io/v1`（OpenAI 兼容面）。
    pub fn minimax(api_key: impl Into<SecretString>) -> Self {
        Self::with_models(
            MINIMAX_BASE_URL,
            ProviderName::MiniMax,
            api_key,
            catalog::models_for(&ProviderName::MiniMax, ProviderKind::Completions),
            HeaderMap::new(),
        )
    }

    /// OpenAI 官方预设：`base_url = https://api.openai.com/v1`。
    ///
    /// `models.rs` 目前没有为 OpenAI 官方提供静态模型列表（其模型迭代速度快，
    /// 硬编码容易过期），因此 `known_models()` 返回空列表；调用方应优先使用
    /// `list_models()` 动态发现，或自行通过 `with_models` 传入。
    pub fn openai(api_key: impl Into<SecretString>) -> Self {
        Self::with_models(
            OPENAI_BASE_URL,
            ProviderName::OpenAI,
            api_key,
            Vec::new(),
            HeaderMap::new(),
        )
    }

    /// Encodes a `Request` into the JSON request body that goes out: the
    /// standard wire JSON from `encoder::encode_request`, then every key of
    /// `options.provider_options` merged into it (Requirement 7.1). A
    /// `provider_options` key that names a standard field overwrites it — the
    /// design calls that "override", not an error.
    ///
    /// The `encoder` does not read `provider_options` (Requirement 7.2); an
    /// empty `provider_options` adds no field at all (Requirement 7.3).
    pub(crate) fn build_body(
        &self,
        req: &Request,
        options: &CallOptions,
        model: Option<&ModelInfo>,
        stream: bool,
    ) -> Result<serde_json::Value, ProviderError> {
        let reasoning = self.endpoint.reasoning_channels(options, model);
        let wire = encoder::encode_request(req, options, reasoning, stream)?;
        let body = serde_json::to_value(wire).map_err(|err| {
            ProviderError::CompletionsEncode(
                super::encoder::CompletionsEncodeError::InvalidContent(err.to_string()),
            )
        })?;
        let body = Endpoint::merge_extra_fields(body, &self.endpoint.extra_fields());
        Ok(Endpoint::merge_provider_options(
            body,
            &options.provider_options,
        ))
    }

    async fn post_chat_completions(
        &self,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, ProviderError> {
        self.endpoint
            .post("chat/completions", body, |reason| {
                ProviderError::CompletionsEncode(
                    super::encoder::CompletionsEncodeError::InvalidContent(reason),
                )
            })
            .await
    }
}

#[async_trait]
impl Provider for CompletionsProvider {
    /// 发送一次非流式请求（Requirement 6.1：强制 `stream = false`）。
    async fn complete(
        &self,
        req: &Request,
        options: &CallOptions,
    ) -> Result<Response, ProviderError> {
        let model = self.resolve_model(&options.model);
        self.endpoint
            .validate_known_model(req, options, model, false)?;
        let body = self.build_body(req, options, model, false)?;
        let response = self.post_chat_completions(&body).await?;

        let body_bytes = response.bytes().await?;
        let wire: ChatCompletionResponse = serde_json::from_slice(&body_bytes).map_err(|e| {
            ProviderError::CompletionsDecode(super::decoder::CompletionsDecodeError::Json(e))
        })?;

        decoder::decode_response(wire).map_err(ProviderError::from)
    }

    /// 发送一次流式请求（Requirement 6.2：强制 `stream = true`），基于
    /// `eventsource-stream` 解析 SSE 字节流，逐个事件委托给 `StreamDecoder`
    /// （Requirement 5.8：连接在 `[DONE]` 之前终止时产出
    /// `ProviderError::Stream`，不伪造 `MessageStop`）。
    async fn stream(
        &self,
        req: &Request,
        options: &CallOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        let model = self.resolve_model(&options.model);
        self.endpoint
            .validate_known_model(req, options, model, true)?;
        let body = self.build_body(req, options, model, true)?;
        let response = self.post_chat_completions(&body).await?;

        let byte_stream = Box::pin(response.bytes_stream());
        let mut sse_stream = byte_stream.eventsource();

        let stream = async_stream::stream! {
            let mut decoder_state = StreamDecoder::new();

            while let Some(event) = sse_stream.next().await {
                let event = match event {
                    Ok(event) => event,
                    Err(err) => {
                        yield Err(ProviderError::Stream(
                            crate::domain::StreamCollectorError::Stream(err.to_string()),
                        ));
                        return;
                    }
                };

                if event.data == "[DONE]" {
                    match decoder_state.finish() {
                        Ok(events) => {
                            for stream_event in events {
                                yield Ok(stream_event);
                            }
                        }
                        Err(err) => {
                            yield Err(ProviderError::CompletionsDecode(err));
                        }
                    }
                    return;
                }

                let chunk: ChatCompletionChunk = match serde_json::from_str(&event.data) {
                    Ok(chunk) => chunk,
                    Err(err) => {
                        yield Err(ProviderError::CompletionsDecode(super::decoder::CompletionsDecodeError::Json(err)));
                        return;
                    }
                };

                match decoder_state.decode_chunk(chunk) {
                    Ok(events) => {
                        for stream_event in events {
                            yield Ok(stream_event);
                        }
                    }
                    Err(err) => {
                        yield Err(ProviderError::CompletionsDecode(err));
                        return;
                    }
                }
            }

            // The loop only exits normally (without an early `return`) when the
            // underlying SSE stream ended without ever seeing `[DONE]`.
            yield Err(ProviderError::Stream(
                crate::domain::StreamCollectorError::Stream(
                    "stream ended before [DONE]".to_string(),
                ),
            ));
        };

        Ok(stream.boxed())
    }

    fn known_models(&self) -> Vec<ModelInfo> {
        self.endpoint.known_models()
    }

    fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
        self.endpoint.resolve_model(id)
    }

    fn protocol(&self) -> Option<crate::ProviderKind> {
        Some(crate::ProviderKind::Completions)
    }

    fn provider_name(&self) -> ProviderName {
        self.endpoint.provider_name.clone()
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        self.endpoint
            .list_models(|err| {
                ProviderError::CompletionsDecode(super::decoder::CompletionsDecodeError::Json(err))
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::message::{ContentBlock, Message};
    use crate::domain::reasoning::ReasoningEffort;
    use crate::domain::{StopReason, StreamCollector, Usage};
    use crate::provider::http;

    fn deepseek_models() -> Vec<ModelInfo> {
        catalog::models_for(&ProviderName::DeepSeek, ProviderKind::Completions)
    }

    fn moonshot_models() -> Vec<ModelInfo> {
        catalog::models_for(&ProviderName::Moonshot, ProviderKind::Completions)
    }

    fn zhipu_models() -> Vec<ModelInfo> {
        catalog::models_for(&ProviderName::Zhipu, ProviderKind::Completions)
    }
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderName, HeaderValue};
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn api_key(value: &str) -> SecretString {
        SecretString::new(value.to_string().into())
    }

    fn sample_request() -> Request {
        Request {
            messages: vec![Message::user(vec![ContentBlock::text("hi")])],
            ..Default::default()
        }
    }

    fn sample_options() -> CallOptions {
        CallOptions::new("gpt-4")
    }

    /// The wire body of one call. The model is resolved against the catalog
    /// once, and both the validation and the reasoning channels read that
    /// same metadata.
    fn wire_body(
        provider: &CompletionsProvider,
        req: &Request,
        options: &CallOptions,
    ) -> serde_json::Value {
        let model = provider.resolve_model(&options.model);
        provider.build_body(req, options, model, false).unwrap()
    }

    // --- constructors (Requirement 6.1, 6.2) ---

    #[test]
    fn new_sets_base_url_and_provider_name() {
        let provider = CompletionsProvider::with_base_url(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        assert_eq!(provider.endpoint.base_url, "https://example.com");
        assert_eq!(
            provider.endpoint.provider_name,
            ProviderName::Custom("example".into())
        );
        assert!(provider.known_models().is_empty());
    }

    #[test]
    fn deepseek_preset_has_correct_base_url_and_models() {
        let provider = CompletionsProvider::deepseek(api_key("k"));
        assert_eq!(provider.endpoint.base_url, "https://api.deepseek.com");
        assert_eq!(provider.endpoint.provider_name, ProviderName::DeepSeek);
        assert_eq!(provider.known_models().len(), deepseek_models().len());
    }

    #[test]
    fn moonshot_preset_has_correct_base_url_and_models() {
        let provider = CompletionsProvider::moonshot(api_key("k"));
        assert_eq!(provider.endpoint.base_url, "https://api.moonshot.cn/v1");
        assert_eq!(provider.endpoint.provider_name, ProviderName::Moonshot);
        assert_eq!(provider.known_models().len(), moonshot_models().len());
    }

    #[test]
    fn zhipu_preset_has_correct_base_url_and_models() {
        let provider = CompletionsProvider::zhipu(api_key("k"));
        assert_eq!(
            provider.endpoint.base_url,
            "https://open.bigmodel.cn/api/paas/v4"
        );
        assert_eq!(provider.endpoint.provider_name, ProviderName::Zhipu);
        assert_eq!(provider.known_models().len(), zhipu_models().len());
    }

    #[test]
    fn openai_preset_has_correct_base_url_and_empty_models() {
        let provider = CompletionsProvider::openai(api_key("k"));
        assert_eq!(provider.endpoint.base_url, "https://api.openai.com/v1");
        assert_eq!(provider.endpoint.provider_name, ProviderName::OpenAI);
        assert!(provider.known_models().is_empty());
    }

    // --- ProviderName dispatch (new) ---

    #[test]
    fn new_dispatches_openai() {
        let provider = CompletionsProvider::new(ProviderName::OpenAI, api_key("k")).unwrap();
        assert_eq!(provider.endpoint.base_url, "https://api.openai.com/v1");
        assert_eq!(provider.endpoint.provider_name, ProviderName::OpenAI);
    }

    #[test]
    fn new_dispatches_deepseek() {
        let provider = CompletionsProvider::new(ProviderName::DeepSeek, api_key("k")).unwrap();
        assert_eq!(provider.endpoint.base_url, "https://api.deepseek.com");
        assert_eq!(provider.endpoint.provider_name, ProviderName::DeepSeek);
        assert_eq!(provider.known_models().len(), deepseek_models().len());
    }

    #[test]
    fn new_dispatches_moonshot() {
        let provider = CompletionsProvider::new(ProviderName::Moonshot, api_key("k")).unwrap();
        assert_eq!(provider.endpoint.base_url, "https://api.moonshot.cn/v1");
        assert_eq!(provider.endpoint.provider_name, ProviderName::Moonshot);
        assert_eq!(provider.known_models().len(), moonshot_models().len());
    }

    #[test]
    fn new_dispatches_zhipu() {
        let provider = CompletionsProvider::new(ProviderName::Zhipu, api_key("k")).unwrap();
        assert_eq!(
            provider.endpoint.base_url,
            "https://open.bigmodel.cn/api/paas/v4"
        );
        assert_eq!(provider.endpoint.provider_name, ProviderName::Zhipu);
        assert_eq!(provider.known_models().len(), zhipu_models().len());
    }

    #[test]
    fn new_rejects_anthropic_without_panicking() {
        let err = CompletionsProvider::new(ProviderName::Anthropic, api_key("k"))
            .err()
            .expect("Anthropic has no Completions preset");
        assert!(matches!(
            err,
            ProviderError::UnsupportedProvider {
                kind: ProviderKind::Completions,
                ..
            }
        ));
        assert!(
            err.to_string().contains("Anthropic"),
            "{err} should name the provider"
        );
    }

    #[test]
    fn new_rejects_grok_without_panicking() {
        let err = CompletionsProvider::new(ProviderName::Grok, api_key("k"))
            .err()
            .expect("Grok has no Completions preset");
        assert!(matches!(
            err,
            ProviderError::UnsupportedProvider {
                kind: ProviderKind::Completions,
                ..
            }
        ));
        assert!(
            err.to_string().contains("Grok"),
            "{err} should name the provider"
        );
    }

    #[test]
    fn new_rejects_custom_without_panicking() {
        let err = CompletionsProvider::new(ProviderName::Custom("example".into()), api_key("k"))
            .err()
            .expect("Custom has no Completions preset");
        assert!(matches!(
            err,
            ProviderError::UnsupportedProvider {
                kind: ProviderKind::Completions,
                ..
            }
        ));
        assert!(
            err.to_string().contains("example"),
            "{err} should name the provider"
        );
    }

    // --- build_headers ---

    #[test]
    fn build_headers_includes_authorization_and_content_type() {
        let provider = CompletionsProvider::with_base_url(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("secret-token"),
        );
        let headers =
            http::build_headers(&provider.endpoint.api_key, &provider.endpoint.extra_headers);
        assert_eq!(headers.get(AUTHORIZATION).unwrap(), "Bearer secret-token");
        assert_eq!(headers.get(CONTENT_TYPE).unwrap(), "application/json");
    }

    #[test]
    fn build_headers_merges_extra_headers() {
        let mut extra = HeaderMap::new();
        extra.insert(
            HeaderName::from_static("x-custom"),
            HeaderValue::from_static("value"),
        );
        let provider = CompletionsProvider::with_models(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("k"),
            Vec::new(),
            extra,
        );
        let headers =
            http::build_headers(&provider.endpoint.api_key, &provider.endpoint.extra_headers);
        assert_eq!(headers.get("x-custom").unwrap(), "value");
    }

    // --- endpoint ---

    #[test]
    fn endpoint_joins_base_url_without_trailing_slash() {
        let provider = CompletionsProvider::with_base_url(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        assert_eq!(
            http::endpoint(&provider.endpoint.base_url, "chat/completions"),
            "https://example.com/chat/completions"
        );
    }

    #[test]
    fn endpoint_handles_trailing_and_leading_slashes() {
        let provider = CompletionsProvider::with_base_url(
            "https://example.com/",
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        assert_eq!(
            http::endpoint(&provider.endpoint.base_url, "/chat/completions"),
            "https://example.com/chat/completions"
        );
    }

    // --- build_body (Requirement 7.1, 7.2, 7.3) ---

    #[test]
    fn build_body_with_empty_provider_options_has_no_extra_fields() {
        let provider = CompletionsProvider::with_base_url(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let req = sample_request();
        let options = sample_options();
        let body = wire_body(&provider, &req, &options);
        let object = body.as_object().unwrap();
        assert!(!object.contains_key("top_k"));
        assert_eq!(object["model"], "gpt-4");
    }

    #[test]
    fn a_zhipu_request_sends_the_thinking_switch_only() {
        let provider = CompletionsProvider::zhipu(api_key("k"));
        let req = sample_request();
        let options = CallOptions::new("glm-5.3").reasoning_effort(ReasoningEffort::High);
        let body = wire_body(&provider, &req, &options);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn a_deepseek_request_sends_the_level_only() {
        let provider = CompletionsProvider::deepseek(api_key("k"));
        let req = sample_request();
        let options = CallOptions::new("deepseek-v4-flash").reasoning_effort(ReasoningEffort::High);
        let body = wire_body(&provider, &req, &options);
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn a_level_the_model_does_not_have_is_clamped_down() {
        let provider = CompletionsProvider::deepseek(api_key("k"));
        let req = sample_request();
        let options =
            CallOptions::new("deepseek-v4-flash").reasoning_effort(ReasoningEffort::Medium);
        let body = wire_body(&provider, &req, &options);
        assert_eq!(body["reasoning_effort"], "low");
    }

    #[test]
    fn an_unset_effort_sends_no_reasoning_field() {
        let provider = CompletionsProvider::zhipu(api_key("k"));
        let req = sample_request();
        let options = CallOptions::new("glm-5.3");
        let body = wire_body(&provider, &req, &options);
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn a_minimax_request_always_splits_the_reasoning_out() {
        let provider = CompletionsProvider::minimax(api_key("k"));
        let req = sample_request();
        let options = sample_options();
        let body = wire_body(&provider, &req, &options);
        assert_eq!(body["reasoning_split"], true);
    }

    #[test]
    fn a_minimax_switch_only_model_uses_the_switch() {
        let provider = CompletionsProvider::minimax(api_key("k"));
        let req = sample_request();
        let options = CallOptions::new("MiniMax-M3").reasoning_effort(ReasoningEffort::Off);
        let body = wire_body(&provider, &req, &options);
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn a_minimax_level_model_uses_the_level() {
        let provider = CompletionsProvider::minimax(api_key("k"));
        let req = sample_request();
        let options =
            CallOptions::new("MiniMax-M3.1-Flash").reasoning_effort(ReasoningEffort::High);
        let body = wire_body(&provider, &req, &options);
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn build_body_merges_provider_options() {
        let provider = CompletionsProvider::with_base_url(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let req = sample_request();
        let options = sample_options().provider_option("top_k", json!(40));
        let body = wire_body(&provider, &req, &options);
        assert_eq!(body["top_k"], 40);
    }

    #[test]
    fn build_body_provider_options_override_standard_field() {
        let provider = CompletionsProvider::with_base_url(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let req = sample_request();
        let options = sample_options()
            .temperature(0.5)
            .provider_option("temperature", json!(1.5));
        let body = wire_body(&provider, &req, &options);
        assert_eq!(body["temperature"], 1.5);
    }

    // --- status code mapping (Requirement 6.3, 6.4, 6.5) via wiremock ---

    #[tokio::test]
    async fn complete_maps_401_to_auth_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
            .mount(&mock_server)
            .await;

        let provider = CompletionsProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let err = provider
            .complete(&sample_request(), &sample_options())
            .await
            .unwrap_err();
        match err {
            ProviderError::Auth(body) => assert_eq!(body, "bad key"),
            other => panic!("expected Auth error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn complete_maps_403_to_auth_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&mock_server)
            .await;

        let provider = CompletionsProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let err = provider
            .complete(&sample_request(), &sample_options())
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::Auth(_)));
    }

    #[tokio::test]
    async fn complete_maps_429_to_rate_limit_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&mock_server)
            .await;

        let provider = CompletionsProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let err = provider
            .complete(&sample_request(), &sample_options())
            .await
            .unwrap_err();
        match err {
            ProviderError::RateLimit { retry_after_ms } => assert_eq!(retry_after_ms, None),
            other => panic!("expected RateLimit error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn complete_maps_other_non_2xx_to_api_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&mock_server)
            .await;

        let provider = CompletionsProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let err = provider
            .complete(&sample_request(), &sample_options())
            .await
            .unwrap_err();
        match err {
            ProviderError::Api { status, body } => {
                assert_eq!(status, 500);
                assert_eq!(body, "boom");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    // --- complete forces stream = false ---

    #[tokio::test]
    async fn complete_sends_stream_false_regardless_of_request() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-1",
                "model": "gpt-4",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "hi" },
                    "finish_reason": "stop"
                }]
            })))
            .mount(&mock_server)
            .await;

        let provider = CompletionsProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let req = sample_request();

        let response = provider.complete(&req, &sample_options()).await.unwrap();
        assert_eq!(response.id, "chatcmpl-1");

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let sent_body: serde_json::Value = requests[0].body_json().unwrap();
        assert_eq!(sent_body["stream"], false);
    }

    // --- known_models ---

    #[test]
    fn known_models_returns_configured_list() {
        let mut models = deepseek_models();
        let provider = CompletionsProvider::with_models(
            "https://example.com",
            ProviderName::DeepSeek,
            api_key("k"),
            models.clone(),
            HeaderMap::new(),
        );
        let mut got = provider.known_models();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        got.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(got, models);
    }

    // --- list_models (Requirement 6.6) ---

    #[tokio::test]
    async fn list_models_returns_minimal_model_info() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    { "id": "gpt-4", "object": "model" },
                    { "id": "gpt-3.5-turbo", "object": "model" }
                ]
            })))
            .mount(&mock_server)
            .await;

        let provider = CompletionsProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let models = provider.list_models().await.unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-4");
        assert_eq!(models[0].provider, ProviderName::Custom("example".into()));
        assert_eq!(
            models[0],
            ModelInfo::minimal("gpt-4", ProviderName::Custom("example".into()))
        );
        assert_eq!(models[1].id, "gpt-3.5-turbo");
    }

    #[tokio::test]
    async fn list_models_maps_error_status_codes() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(401).set_body_string("nope"))
            .mount(&mock_server)
            .await;

        let provider = CompletionsProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let err = provider.list_models().await.unwrap_err();
        assert!(matches!(err, ProviderError::Auth(_)));
    }

    // --- stream forces stream = true, terminates on [DONE] ---

    #[tokio::test]
    async fn stream_sends_stream_true_and_decodes_events_until_done() {
        let mock_server = MockServer::start().await;
        let sse_body = concat!(
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_body),
            )
            .mount(&mock_server)
            .await;

        let provider = CompletionsProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let req = sample_request();

        let mut event_stream = provider.stream(&req, &sample_options()).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = event_stream.next().await {
            events.push(event.unwrap());
        }

        assert!(matches!(events[0], StreamEvent::MessageStart { .. }));
        assert!(matches!(events.last().unwrap(), StreamEvent::MessageStop));

        let requests = mock_server.received_requests().await.unwrap();
        let sent_body: serde_json::Value = requests[0].body_json().unwrap();
        assert_eq!(sent_body["stream"], true);
    }

    #[tokio::test]
    async fn stream_collects_usage_from_trailing_chunk() {
        let mock_server = MockServer::start().await;
        let sse_body = concat!(
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4\",\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":11,\"total_tokens\":18}}\n\n",
            "data: [DONE]\n\n"
        );
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_body),
            )
            .mount(&mock_server)
            .await;

        let provider = CompletionsProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let mut event_stream = provider
            .stream(&sample_request(), &sample_options())
            .await
            .unwrap();
        let mut collector = StreamCollector::new();
        while let Some(event) = event_stream.next().await {
            collector.push(&event.unwrap());
        }

        let response = collector.finish().unwrap();
        assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(
            response.usage,
            Some(Usage {
                input_tokens: 7,
                output_tokens: 11,
                cache_read_tokens: 0,
                reasoning_tokens: 0,
            })
        );
    }

    #[tokio::test]
    async fn stream_yields_error_when_connection_ends_before_done() {
        let mock_server = MockServer::start().await;
        let sse_body = "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n";
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_body),
            )
            .mount(&mock_server)
            .await;

        let provider = CompletionsProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let mut event_stream = provider
            .stream(&sample_request(), &sample_options())
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(event) = event_stream.next().await {
            events.push(event);
        }

        let last = events.last().unwrap();
        match last {
            Err(ProviderError::Stream(err)) => {
                assert_eq!(err.to_string(), "stream ended before [DONE]");
            }
            other => panic!("expected Stream error, got {other:?}"),
        }
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(StreamEvent::MessageStop)))
        );
    }

    #[tokio::test]
    async fn stream_yields_error_when_the_reply_ends_without_a_finish_reason() {
        let mock_server = MockServer::start().await;
        let sse_body = concat!(
            "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"thinking\"},\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n"
        );
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_body),
            )
            .mount(&mock_server)
            .await;

        let provider = CompletionsProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let mut event_stream = provider
            .stream(&sample_request(), &sample_options())
            .await
            .unwrap();
        let mut events = Vec::new();
        while let Some(event) = event_stream.next().await {
            events.push(event);
        }

        let last = events.last().unwrap();
        match last {
            Err(ProviderError::CompletionsDecode(err)) => {
                assert_eq!(err.to_string(), "stream ended before the model finished");
            }
            other => panic!("expected a decode error, got {other:?}"),
        }
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Ok(StreamEvent::MessageStop))),
            "a cut-off reply must not look finished"
        );
    }

    #[tokio::test]
    async fn stream_maps_error_status_codes() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&mock_server)
            .await;

        let provider = CompletionsProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let result = provider.stream(&sample_request(), &sample_options()).await;
        match result {
            Err(ProviderError::RateLimit { .. }) => {}
            Err(other) => panic!("expected RateLimit error, got {other:?}"),
            Ok(_) => panic!("expected an error, got Ok"),
        }
    }
}

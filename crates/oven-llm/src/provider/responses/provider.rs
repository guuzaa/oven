//! `OpenAIResponsesProvider`：OpenAI Responses API 协议的 `Provider`
//! 实现（传输层）。
//!
//! 本模块负责把 domain 层的 `Request`/`Response`/`StreamEvent` 与
//! `encoder`/`decoder` 串联起来，完成实际的 HTTP 调用：
//! - `build_body`：`encoder::encode_request` + `provider_options` 合并
//! - `post_responses`：POST `responses`，把非 2xx 状态码映射为具体的
//!   `ProviderError` 变体
//! - `Provider::complete` / `Provider::stream`：分别对接非流式响应解码与
//!   基于 `eventsource-stream` 的 SSE 流式解码
//! - `Provider::known_models` / `Provider::list_models`：模型元数据查询

use std::collections::HashMap;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use futures::stream::BoxStream;
use isahc::http::header::HeaderMap;
use isahc::prelude::*;
use secrecy::SecretString;

use super::decoder::{self, StreamDecoder};
use super::encoder;
use super::models::{deepseek_models, grok_models};
use super::types::{ResponseEvent, ResponseObject};
use crate::ProviderName;
use crate::domain::{ModelId, Request, Response, StreamEvent};
use crate::provider::http;
use crate::provider::model::ModelInfo;
use crate::provider::validate::validate_request;
use crate::provider::{DEEPSEEK_BASE_URL, GROK_BASE_URL, OPENAI_BASE_URL, Provider, ProviderError};

/// OpenAI Responses API 协议的通用 `Provider` 实现。
///
/// 通过 `base_url` 区分具体服务商（DeepSeek / Grok / OpenAI 官方等），wire
/// 格式（`types.rs`/`encoder.rs`/`decoder.rs`）在这些服务商之间完全共享。
pub struct ResponsesProvider {
    base_url: String,
    provider_name: ProviderName,
    api_key: SecretString,
    extra_headers: HeaderMap,
    model_catalog: HashMap<ModelId, ModelInfo>,
    client: isahc::HttpClient,
}

impl ResponsesProvider {
    /// 通过 `ProviderName` 派发构造。已知的 Responses API 服务商会自动填入
    /// 对应的 `base_url` 与模型元数据；对其他服务商，请改用 [`with_base_url`]
    /// 或 [`with_models`]。
    pub fn new(provider_name: ProviderName, api_key: impl Into<SecretString>) -> Self {
        match &provider_name {
            ProviderName::OpenAI => Self::openai(api_key),
            ProviderName::DeepSeek => Self::deepseek(api_key),
            ProviderName::Grok => Self::grok(api_key),
            ProviderName::Anthropic => {
                panic!(
                    "Anthropic ({provider_name:?}) does not provide a Responses API; \
                     use the Anthropic-specific provider instead of OpenAIResponsesProvider"
                )
            }
            ProviderName::Moonshot | ProviderName::Zhipu => {
                panic!(
                    "{provider_name:?} has no Responses API preset yet; \
                     use OpenAIResponsesProvider::with_base_url or ::with_models instead"
                )
            }
            ProviderName::Custom(name) => {
                panic!(
                    "cannot dispatch to custom provider '{name}'; \
                     use OpenAIResponsesProvider::with_base_url or ::with_models instead"
                )
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
        let model_catalog = known_models
            .into_iter()
            .map(|model| (ModelId::from(model.id.as_str()), model))
            .collect();

        Self {
            base_url: base_url.into(),
            provider_name,
            api_key: api_key.into(),
            extra_headers,
            model_catalog,
            client: isahc::HttpClient::new().expect("isahc HttpClient::new() should succeed"),
        }
    }

    /// DeepSeek 预设：`base_url = https://api.deepseek.com`，目录含
    /// `deepseek-v4-flash`（官方文档注明 `deepseek-v4-pro` 暂不支持
    /// Responses API）。
    pub fn deepseek(api_key: impl Into<SecretString>) -> Self {
        Self::with_models(
            DEEPSEEK_BASE_URL,
            ProviderName::DeepSeek,
            api_key,
            deepseek_models(),
            HeaderMap::new(),
        )
    }

    /// Grok（xAI）预设：`base_url = https://api.x.ai/v1`，目录含
    /// `grok-build-0.1`。
    pub fn grok(api_key: impl Into<SecretString>) -> Self {
        Self::with_models(
            GROK_BASE_URL,
            ProviderName::Grok,
            api_key,
            grok_models(),
            HeaderMap::new(),
        )
    }

    /// OpenAI 官方预设：`base_url = https://api.openai.com/v1`，空模型目录
    /// （官方模型迭代快，硬编码容易过期，调用方应优先使用 `list_models()`
    /// 或自行通过 `with_models` 传入）。
    pub fn openai(api_key: impl Into<SecretString>) -> Self {
        Self::with_models(
            OPENAI_BASE_URL,
            ProviderName::OpenAI,
            api_key,
            Vec::new(),
            HeaderMap::new(),
        )
    }

    /// 将 `Request` 编码为最终发送的 JSON 请求体：先由 `encoder::encode_request`
    /// 产出标准 wire JSON，再把 `req.provider_options` 中的每个键值对合并进
    /// 去。`provider_options` 的键与标准字段重名时会覆盖标准字段。
    fn build_body(&self, req: &Request, stream: bool) -> Result<serde_json::Value, ProviderError> {
        let wire = encoder::encode_request(req, stream)?;

        let mut body = serde_json::to_value(wire).map_err(|err| {
            ProviderError::ResponsesEncode(encoder::EncodeError::InvalidContent(err.to_string()))
        })?;

        if !req.provider_options.is_empty() {
            let object = body
                .as_object_mut()
                .expect("CreateResponseRequest always serializes to a JSON object");
            for (key, value) in req.provider_options.iter() {
                object.insert(key.clone(), value.clone());
            }
        }

        Ok(body)
    }

    /// 对静态目录命中的模型执行调用模式相关校验；未命中时按宽松策略透传，
    /// 交由上游服务决定其可用性与能力约束。
    fn validate_known_model(&self, req: &Request, stream: bool) -> Result<(), ProviderError> {
        if let Some(model) = self.resolve_model(&req.model) {
            validate_request(req, model, stream)?;
        }
        Ok(())
    }

    /// POST `body` 到 `{base_url}/responses`，返回响应的原始 body；非 2xx
    /// 状态码映射为具体的 `ProviderError`。
    async fn post_responses(
        &self,
        body: &serde_json::Value,
    ) -> Result<isahc::Response<isahc::AsyncBody>, ProviderError> {
        let body_bytes = serde_json::to_vec(body).map_err(|e| {
            ProviderError::ResponsesEncode(encoder::EncodeError::InvalidContent(e.to_string()))
        })?;

        let headers = http::build_headers(&self.api_key, &self.extra_headers);
        let url = http::endpoint(&self.base_url, "responses");
        http::post_json(&self.client, &headers, url, body_bytes).await
    }
}

#[async_trait]
impl Provider for ResponsesProvider {
    /// 发送一次非流式请求（强制 `stream = false`）。
    async fn complete(&self, req: &Request) -> Result<Response, ProviderError> {
        self.validate_known_model(req, false)?;
        let body = self.build_body(req, false)?;
        let mut response = self.post_responses(&body).await?;

        let body_bytes = response.bytes().await?;
        let wire: ResponseObject = serde_json::from_slice(&body_bytes)
            .map_err(|e| ProviderError::ResponsesDecode(decoder::DecodeError::Json(e)))?;

        decoder::decode_response(wire).map_err(ProviderError::from)
    }

    /// 发送一次流式请求（强制 `stream = true`），基于 `eventsource-stream`
    /// 解析 SSE 字节流，逐个事件委托给 `StreamDecoder`。Responses API 没有
    /// `[DONE]` 标记：终止事件是 `response.completed` / `response.incomplete`；
    /// 连接在终止事件之前结束时会产出 `ProviderError::Stream`，不伪造
    /// `MessageStop`。
    async fn stream(
        &self,
        req: &Request,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        self.validate_known_model(req, true)?;
        let body = self.build_body(req, true)?;
        let response = self.post_responses(&body).await?;

        let byte_stream = Box::pin(http::body_to_stream(response.into_body()));
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

                if event.data.trim().is_empty() {
                    continue;
                }

                let wire_event: ResponseEvent = match serde_json::from_str(&event.data) {
                    Ok(event) => event,
                    Err(err) => {
                        yield Err(ProviderError::ResponsesDecode(
                            decoder::DecodeError::Json(err),
                        ));
                        return;
                    }
                };

                match decoder_state.decode_event(wire_event) {
                    Ok(events) => {
                        for stream_event in events {
                            yield Ok(stream_event);
                        }
                        if decoder_state.is_awaiting_done() {
                            match decoder_state.finish() {
                                Ok(events) => {
                                    for stream_event in events {
                                        yield Ok(stream_event);
                                    }
                                }
                                Err(err) => {
                                    yield Err(ProviderError::ResponsesDecode(err));
                                }
                            }
                            return;
                        }
                    }
                    Err(err) => {
                        yield Err(ProviderError::ResponsesDecode(err));
                        return;
                    }
                }
            }

            // The loop only exits normally (without an early `return`) when the
            // underlying SSE stream ended without ever seeing a terminal event.
            yield Err(ProviderError::Stream(
                crate::domain::StreamCollectorError::Stream(
                    "stream ended before response.completed".to_string(),
                ),
            ));
        };

        Ok(stream.boxed())
    }

    /// 该 provider 静态已知的模型列表（构造时传入，不涉及网络）。
    fn known_models(&self) -> Vec<ModelInfo> {
        self.model_catalog.values().cloned().collect()
    }

    /// 通过构造时建立的哈希索引查找静态模型元数据。
    fn resolve_model(&self, id: &ModelId) -> Option<&ModelInfo> {
        self.model_catalog
            .get(id)
            .or_else(|| self.model_catalog.get(&ModelId::from(id.wire_id())))
    }

    fn protocol(&self) -> Option<crate::ProviderKind> {
        Some(crate::ProviderKind::Responses)
    }

    fn provider_name(&self) -> ProviderName {
        self.provider_name.clone()
    }

    /// GET `{base_url}/models`，仅填充 `id` 与 `provider` 字段。
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let headers = http::build_headers(&self.api_key, &self.extra_headers);
        http::list_models(
            &self.client,
            &headers,
            &self.base_url,
            &self.provider_name,
            |e| ProviderError::ResponsesDecode(decoder::DecodeError::Json(e)),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::super::testdata::deepseek_sse;
    use super::*;
    use crate::domain::message::{ContentBlock, Message};
    use crate::domain::stream::StreamCollector;
    use isahc::http::header::{AUTHORIZATION, CONTENT_TYPE, HeaderName, HeaderValue};
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn api_key(value: &str) -> SecretString {
        SecretString::new(value.to_string().into())
    }

    fn sample_request() -> Request {
        Request {
            model: ModelId::from("deepseek-v4-flash"),
            messages: vec![Message::user(vec![ContentBlock::text("hi")])],
            ..Default::default()
        }
    }

    // --- constructors ---

    #[test]
    fn new_dispatches_openai() {
        let provider = ResponsesProvider::new(ProviderName::OpenAI, api_key("k"));
        assert_eq!(provider.base_url, "https://api.openai.com/v1");
        assert_eq!(provider.provider_name, ProviderName::OpenAI);
        assert!(provider.known_models().is_empty());
    }

    #[test]
    fn new_dispatches_deepseek() {
        let provider = ResponsesProvider::new(ProviderName::DeepSeek, api_key("k"));
        assert_eq!(provider.base_url, "https://api.deepseek.com");
        assert_eq!(provider.provider_name, ProviderName::DeepSeek);
        assert_eq!(provider.known_models().len(), deepseek_models().len());
    }

    #[test]
    fn new_dispatches_grok() {
        let provider = ResponsesProvider::new(ProviderName::Grok, api_key("k"));
        assert_eq!(provider.base_url, "https://api.x.ai/v1");
        assert_eq!(provider.provider_name, ProviderName::Grok);
        assert_eq!(provider.known_models().len(), grok_models().len());
    }

    #[test]
    #[should_panic(expected = "Anthropic")]
    fn new_panics_on_anthropic() {
        let _ = ResponsesProvider::new(ProviderName::Anthropic, api_key("k"));
    }

    #[test]
    #[should_panic(expected = "cannot dispatch")]
    fn new_panics_on_custom() {
        let _ = ResponsesProvider::new(ProviderName::Custom("example".into()), api_key("k"));
    }

    #[test]
    fn with_base_url_sets_fields() {
        let provider = ResponsesProvider::with_base_url(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        assert_eq!(provider.base_url, "https://example.com");
        assert_eq!(
            provider.provider_name,
            ProviderName::Custom("example".into())
        );
        assert!(provider.known_models().is_empty());
    }

    // --- build_headers / endpoint ---

    #[test]
    fn build_headers_includes_authorization_and_content_type() {
        let provider = ResponsesProvider::with_base_url(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("secret-token"),
        );
        let headers = http::build_headers(&provider.api_key, &provider.extra_headers);
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
        let provider = ResponsesProvider::with_models(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("k"),
            Vec::new(),
            extra,
        );
        let headers = http::build_headers(&provider.api_key, &provider.extra_headers);
        assert_eq!(headers.get("x-custom").unwrap(), "value");
    }

    #[test]
    fn endpoint_joins_base_url() {
        let provider = ResponsesProvider::with_base_url(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        assert_eq!(
            http::endpoint(&provider.base_url, "responses"),
            "https://example.com/responses"
        );
    }

    #[test]
    fn endpoint_handles_trailing_and_leading_slashes() {
        let provider = ResponsesProvider::with_base_url(
            "https://example.com/",
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        assert_eq!(
            http::endpoint(&provider.base_url, "/responses"),
            "https://example.com/responses"
        );
    }

    // --- build_body + provider_options ---

    #[test]
    fn build_body_with_empty_provider_options_has_no_extra_fields() {
        let provider = ResponsesProvider::with_base_url(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let req = sample_request();
        let body = provider.build_body(&req, false).unwrap();
        let object = body.as_object().unwrap();
        assert_eq!(object["model"], "deepseek-v4-flash");
        assert!(object.get("text").is_none());
    }

    #[test]
    fn build_body_merges_provider_options() {
        let provider = ResponsesProvider::with_base_url(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let mut req = sample_request();
        req.provider_options.insert("top_k".to_string(), json!(40));
        let body = provider.build_body(&req, false).unwrap();
        assert_eq!(body["top_k"], 40);
    }

    #[test]
    fn build_body_provider_options_override_standard_field() {
        let provider = ResponsesProvider::with_base_url(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let mut req = sample_request();
        req.sampling.temperature = Some(0.5);
        req.provider_options
            .insert("temperature".to_string(), json!(1.5));
        let body = provider.build_body(&req, false).unwrap();
        assert_eq!(body["temperature"], 1.5);
    }

    #[test]
    fn build_body_provider_options_can_add_text_format() {
        let provider = ResponsesProvider::with_base_url(
            "https://example.com",
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let mut req = sample_request();
        req.provider_options.insert(
            "text".to_string(),
            json!({"format": {"type": "json_object"}}),
        );
        let body = provider.build_body(&req, false).unwrap();
        assert_eq!(body["text"]["format"]["type"], "json_object");
    }

    // --- status code mapping ---

    #[tokio::test]
    async fn complete_maps_401_to_auth_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
            .mount(&mock_server)
            .await;

        let provider = ResponsesProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let err = provider.complete(&sample_request()).await.unwrap_err();
        match err {
            ProviderError::Auth(body) => assert_eq!(body, "bad key"),
            other => panic!("expected Auth error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn complete_maps_429_to_rate_limit_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&mock_server)
            .await;

        let provider = ResponsesProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let err = provider.complete(&sample_request()).await.unwrap_err();
        match err {
            ProviderError::RateLimit { retry_after_ms } => assert_eq!(retry_after_ms, None),
            other => panic!("expected RateLimit error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn complete_maps_500_to_api_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&mock_server)
            .await;

        let provider = ResponsesProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let err = provider.complete(&sample_request()).await.unwrap_err();
        match err {
            ProviderError::Api { status, body } => {
                assert_eq!(status, 500);
                assert_eq!(body, "boom");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    // --- complete（非流式）---

    #[tokio::test]
    async fn complete_decodes_full_response() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_1",
                "model": "deepseek-v4-flash",
                "status": "completed",
                "error": null,
                "incomplete_details": null,
                "output": [
                    {
                        "type": "reasoning",
                        "id": "rs_1",
                        "summary": [{"type": "summary_text", "text": "let me think"}]
                    },
                    {
                        "type": "message",
                        "id": "msg_1",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "hi"}]
                    }
                ],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "input_tokens_details": {"cached_tokens": 7},
                    "output_tokens_details": {"reasoning_tokens": 3}
                }
            })))
            .mount(&mock_server)
            .await;

        let provider = ResponsesProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let response = provider.complete(&sample_request()).await.unwrap();
        assert_eq!(response.id, "resp_1");
        assert_eq!(response.model, "deepseek-v4-flash");
        assert_eq!(response.content.len(), 2);
        assert!(
            matches!(&response.content[0], ContentBlock::Thinking { thinking } if thinking == "let me think")
        );
        assert!(matches!(&response.content[1], ContentBlock::Text { text } if text == "hi"));
        assert_eq!(
            response.stop_reason,
            Some(crate::domain::StopReason::EndTurn)
        );
        assert_eq!(
            response.usage,
            Some(crate::domain::Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 7,
                reasoning_tokens: 3,
            })
        );

        let requests = mock_server.received_requests().await.unwrap();
        let sent_body: serde_json::Value = requests[0].body_json().unwrap();
        assert_eq!(sent_body["stream"], false);
        assert_eq!(sent_body["model"], "deepseek-v4-flash");
    }

    #[tokio::test]
    async fn complete_decodes_failed_response_as_error() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_1",
                "model": "deepseek-v4-flash",
                "status": "failed",
                "error": {"code": "server_error", "message": "boom", "param": null},
                "output": []
            })))
            .mount(&mock_server)
            .await;

        let provider = ResponsesProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let err = provider.complete(&sample_request()).await.unwrap_err();
        match err {
            ProviderError::ResponsesDecode(decoder::DecodeError::Failed { message }) => {
                assert_eq!(message, "boom");
            }
            other => panic!("expected ResponsesDecode(Failed), got {other:?}"),
        }
    }

    // --- stream（SSE）---

    #[tokio::test]
    async fn stream_decodes_deepseek_responses_end_to_end() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(deepseek_sse()),
            )
            .mount(&mock_server)
            .await;

        let provider = ResponsesProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let mut event_stream = provider.stream(&sample_request()).await.unwrap();

        let mut collector = StreamCollector::new();
        let mut saw_message_stop = false;
        while let Some(event) = event_stream.next().await {
            match event {
                Ok(stream_event) => {
                    if matches!(stream_event, StreamEvent::MessageStop) {
                        saw_message_stop = true;
                    }
                    collector.push(&stream_event);
                }
                Err(err) => panic!("unexpected stream error: {err:?}"),
            }
        }

        assert!(saw_message_stop);
        let response = collector.finish().unwrap();
        assert_eq!(response.id, "be91d05d-ff00-4efb-b63a-a1a96d13c7a8");
        assert_eq!(response.model, "deepseek-v4-flash");
        assert_eq!(
            response.stop_reason,
            Some(crate::domain::StopReason::ToolUse)
        );
        assert_eq!(response.content.len(), 2);
        assert!(matches!(
            &response.content[0],
            ContentBlock::Text { text } if text == "I'll check the weather in Hangzhou, Zhejiang for you."
        ));
        assert!(matches!(
            &response.content[1],
            ContentBlock::ToolUse { name, input, .. } if name == "get_weather" && input == &json!({})
        ));

        let requests = mock_server.received_requests().await.unwrap();
        let sent_body: serde_json::Value = requests[0].body_json().unwrap();
        assert_eq!(sent_body["stream"], true);
    }

    #[tokio::test]
    async fn stream_yields_error_when_connection_ends_before_terminal_event() {
        let mock_server = MockServer::start().await;
        let sse_body = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\",\"model\":\"m1\",\"output\":[]}}\n\n",
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"m1\",\"role\":\"assistant\",\"content\":[]}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"hi\"}\n\n"
        );
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_body),
            )
            .mount(&mock_server)
            .await;

        let provider = ResponsesProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let mut event_stream = provider.stream(&sample_request()).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = event_stream.next().await {
            events.push(event);
        }

        let last = events.last().unwrap();
        match last {
            Err(ProviderError::Stream(err)) => {
                assert_eq!(
                    err.to_string(),
                    "stream protocol error: stream ended before response.completed"
                );
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
    async fn stream_maps_error_status_codes() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&mock_server)
            .await;

        let provider = ResponsesProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let result = provider.stream(&sample_request()).await;
        match result {
            Err(ProviderError::RateLimit { .. }) => {}
            Err(other) => panic!("expected RateLimit error, got {other:?}"),
            Ok(_) => panic!("expected an error, got Ok"),
        }
    }

    // --- list_models ---

    #[tokio::test]
    async fn list_models_returns_minimal_model_info() {
        let mock_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    { "id": "deepseek-v4-flash", "object": "model" },
                    { "id": "deepseek-v4-pro", "object": "model" }
                ]
            })))
            .mount(&mock_server)
            .await;

        let provider = ResponsesProvider::with_base_url(
            mock_server.uri(),
            ProviderName::Custom("example".into()),
            api_key("k"),
        );
        let models = provider.list_models().await.unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "deepseek-v4-flash");
        assert_eq!(
            models[0],
            ModelInfo::minimal("deepseek-v4-flash", ProviderName::Custom("example".into()))
        );
        assert_eq!(models[1].id, "deepseek-v4-pro");
    }
}

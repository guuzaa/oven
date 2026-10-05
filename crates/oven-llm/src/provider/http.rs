//! provider 间共享的 HTTP/传输层工具。
//!
//! `openai_compat` 与 `responses` 两个 provider 共用同一套 isahc 传输
//! 管道：请求头组装、URL 拼接、POST + 状态码检查、SSE 字节流桥接，以及
//! `GET /models` 模型列表查询。这些逻辑与具体 wire 协议无关，集中在此处
//! 避免重复；协议相关的 encode/decode 错误映射仍留在各 provider 内。

use futures_lite::io::AsyncReadExt;
use isahc::http::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use isahc::prelude::*;
use secrecy::{ExposeSecret, SecretString};

use crate::ProviderName;
use crate::provider::ProviderError;
use crate::provider::model::ModelInfo;

/// 将 `isahc::AsyncBody` 转换为 `Stream<Item = Result<Vec<u8>, io::Error>>`。
///
/// `eventsource-stream` 期望输入为 `Stream`，而 isahc 的响应体实现的是
/// `AsyncRead` —— 因此需要通过此适配器桥接。
pub(crate) fn body_to_stream(
    mut body: isahc::AsyncBody,
) -> impl futures::stream::Stream<Item = Result<Vec<u8>, std::io::Error>> + Send {
    async_stream::stream! {
        let mut buf = vec![0u8; 8192];
        loop {
            match body.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => yield Ok(buf[..n].to_vec()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => yield Err(e),
            }
        }
    }
}

/// 组装标准请求头：`Authorization: Bearer <api_key>` + `Content-Type:
/// application/json`，再合并 `extra_headers`（`extra_headers` 中的同名头会
/// 覆盖前两者）。
pub(crate) fn build_headers(api_key: &SecretString, extra_headers: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let auth_value = format!("Bearer {}", api_key.expose_secret());
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&auth_value)
            .unwrap_or_else(|_| HeaderValue::from_static("Bearer invalid-api-key")),
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    for (name, value) in extra_headers.iter() {
        headers.insert(name.clone(), value.clone());
    }

    headers
}

/// 将 `base_url` 与 `path` 拼接为完整端点 URL，正确处理 `base_url` 末尾和
/// `path` 开头可能存在/不存在的 `/`。
pub(crate) fn endpoint(base_url: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    format!("{base}/{path}")
}

/// 检查响应状态码，非 2xx 时读取响应体并映射为具体的 `ProviderError`；
/// 2xx 时原样返回 `response` 供调用方继续处理（消费其 body/字节流）。
pub(crate) async fn check_status(
    response: isahc::Response<isahc::AsyncBody>,
) -> Result<isahc::Response<isahc::AsyncBody>, ProviderError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let status_code = status.as_u16();
    let mut resp = response;
    let body = resp.text().await.unwrap_or_default();
    match status_code {
        401 | 403 => Err(ProviderError::Auth(body)),
        429 => Err(ProviderError::RateLimit {
            retry_after_ms: None,
        }),
        other => Err(ProviderError::Api {
            status: other,
            body,
        }),
    }
}

/// POST 已序列化的 `body_bytes` 到 `url`，并执行状态码检查。
///
/// 入参是序列化后的字节而非 `serde_json::Value`：`ProviderError` 的编码错误
/// 变体是协议相关的（`Encode` vs `ResponsesEncode`），序列化失败的错误映射
/// 应留在各 provider 内。
pub(crate) async fn post_json(
    client: &isahc::HttpClient,
    headers: &HeaderMap,
    url: String,
    body_bytes: Vec<u8>,
) -> Result<isahc::Response<isahc::AsyncBody>, ProviderError> {
    let mut builder = isahc::http::Request::post(url);
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }
    let request = builder
        .body(body_bytes)
        .expect("valid HTTP request construction");

    let response = client.send_async(request).await?;

    check_status(response).await
}

/// GET `{base_url}/models`，仅填充 `id` 与 `provider` 字段。
///
/// 响应体 JSON 解析失败通过 `map_json_err` 映射为调用方协议对应的
/// `ProviderError::Decode` / `ResponsesDecode` 变体。
pub(crate) async fn list_models(
    client: &isahc::HttpClient,
    headers: &HeaderMap,
    base_url: &str,
    provider_name: &ProviderName,
    map_json_err: impl FnOnce(serde_json::Error) -> ProviderError,
) -> Result<Vec<ModelInfo>, ProviderError> {
    let mut builder = isahc::http::Request::get(endpoint(base_url, "models"));
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }
    let request = builder.body(()).expect("valid HTTP request construction");

    let response = client.send_async(request).await?;
    let mut response = check_status(response).await?;

    let body_bytes = response.bytes().await?;
    let body: ModelsListResponse = serde_json::from_slice(&body_bytes).map_err(map_json_err)?;

    Ok(body
        .data
        .into_iter()
        .map(|entry| ModelInfo::minimal(entry.id, provider_name.clone()))
        .collect())
}

/// `GET /models` 响应体的最小反序列化目标：`{"data": [{"id": "..."}, ...]}`。
#[derive(Debug, serde::Deserialize)]
struct ModelsListResponse {
    data: Vec<ModelsListEntry>,
}

/// `ModelsListResponse.data` 中的单个条目：只关心 `id`，其余字段忽略。
#[derive(Debug, serde::Deserialize)]
struct ModelsListEntry {
    id: String,
}

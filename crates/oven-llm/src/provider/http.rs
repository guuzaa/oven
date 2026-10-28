//! provider 间共享的 HTTP/传输层工具。
//!
//! `openai_compat` 与 `responses` 两个 provider 共用同一套 reqwest 传输
//! 管道：请求头组装、URL 拼接、POST + 状态码检查、SSE 字节流桥接，以及
//! `GET /models` 模型列表查询。这些逻辑与具体 wire 协议无关，集中在此处
//! 避免重复；协议相关的 encode/decode 错误映射仍留在各 provider 内。

use reqwest::Url;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Response, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use crate::ProviderName;
use crate::provider::ProviderError;
use crate::provider::model::ModelInfo;

/// 拒绝请求的理由最多占多少字符。代理的错误页很长，里面那句话不长。
const MAX_REASON_CHARS: usize = 200;

/// `base_url` 拼出的端点不是合法 URL 时返回配置错误。
///
/// 宿主可能从用户配置拿到任意 `base_url`，这里不能 panic。
fn invalid_endpoint(url: &str, reason: impl std::fmt::Display) -> ProviderError {
    ProviderError::InvalidProviderConfig(format!("invalid endpoint url {url}: {reason}"))
}

/// 解析端点 URL，并拒绝 http/https 之外的 scheme。
///
/// reqwest 只支持 http/https；把校验放在发请求之前，畸形 `base_url` 会报成
/// 配置错误而不是传输错误。
fn parse_endpoint(url: &str) -> Result<Url, ProviderError> {
    let parsed = Url::parse(url).map_err(|err| invalid_endpoint(url, err))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(invalid_endpoint(url, "unsupported scheme"));
    }
    Ok(parsed)
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
    response: reqwest::Response,
) -> Result<reqwest::Response, ProviderError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let status_code = status.as_u16();
    let retry_after_ms = retry_after_ms(&response);
    let body = response.text().await.unwrap_or_default();
    let reason = reason_from_body(&body, status);
    match status_code {
        401 | 403 => Err(ProviderError::Auth(reason)),
        429 => Err(ProviderError::RateLimit {
            retry_after_ms,
            body: reason,
        }),
        other => Err(ProviderError::Api {
            status: other,
            body: reason,
        }),
    }
}

/// 厂商让调用方等多久，单位毫秒。
///
/// `Retry-After` 头写的是秒；日期写法少见，读不出来就按自己的节奏等。
fn retry_after_ms(response: &Response) -> Option<u64> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|seconds| seconds.saturating_mul(1000))
}

/// 厂商拒绝这次请求的理由，压成一行。
///
/// 多数厂商回 `{"error":{"message":"..."}}`，要读的是那句话，不是包着它的
/// JSON。代理的 HTML 错误页是另一个极端：整页压成一行，再截到终端一行放得下。
fn reason_from_body(body: &str, status: StatusCode) -> String {
    let text = provider_message(body).unwrap_or_else(|| body.to_string());
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return status
            .canonical_reason()
            .unwrap_or("the provider said nothing")
            .to_string();
    }
    if text.chars().count() > MAX_REASON_CHARS {
        let head: String = text.chars().take(MAX_REASON_CHARS).collect();
        return format!("{head}…");
    }
    text
}

/// 错误 body 里厂商给的那句话（有的话）。
fn provider_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    if let Some(error) = value.get("error") {
        let message = match error {
            Value::String(message) => Some(message.as_str()),
            Value::Object(fields) => fields.get("message")?.as_str(),
            _ => None,
        };
        if let Some(message) = message {
            return Some(message.to_string());
        }
    }
    for key in ["message", "detail"] {
        if let Some(message) = value.get(key).and_then(Value::as_str) {
            return Some(message.to_string());
        }
    }
    None
}

/// POST 已序列化的 `body_bytes` 到 `url`，并执行状态码检查。
///
/// 入参是序列化后的字节而非 `serde_json::Value`：`ProviderError` 的编码错误
/// 变体是协议相关的（`Encode` vs `ResponsesEncode`），序列化失败的错误映射
/// 应留在各 provider 内。
///
/// `url` 不是合法的绝对 HTTP URL（`base_url` 写错）时返回
/// [`ProviderError::InvalidProviderConfig`]，而不是 panic。
pub(crate) async fn post_json(
    client: &reqwest::Client,
    headers: HeaderMap,
    url: String,
    body_bytes: Vec<u8>,
) -> Result<reqwest::Response, ProviderError> {
    let endpoint = parse_endpoint(&url)?;
    let response = client
        .post(endpoint)
        .headers(headers)
        .body(body_bytes)
        .send()
        .await?;

    check_status(response).await
}

/// GET `{base_url}/models`，仅填充 `id` 与 `provider` 字段。
///
/// 响应体 JSON 解析失败通过 `map_json_err` 映射为调用方协议对应的
/// `ProviderError::Decode` / `ResponsesDecode` 变体。
pub(crate) async fn list_models(
    client: &reqwest::Client,
    headers: HeaderMap,
    base_url: &str,
    provider_name: &ProviderName,
    map_json_err: impl FnOnce(serde_json::Error) -> ProviderError,
) -> Result<Vec<ModelInfo>, ProviderError> {
    let url = endpoint(base_url, "models");
    let endpoint = parse_endpoint(&url)?;
    let response = client.get(endpoint).headers(headers).send().await?;
    let response = check_status(response).await?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const MALFORMED_BASE_URL: &str = "http://[::1";
    const NON_HTTP_BASE_URL: &str = "ftp://example.com";
    const PROVIDER: &str = "example";

    fn client() -> reqwest::Client {
        reqwest::Client::new()
    }

    fn map_json_err(err: serde_json::Error) -> ProviderError {
        ProviderError::InvalidProviderConfig(err.to_string())
    }

    #[tokio::test]
    async fn a_refused_request_reports_the_reason_the_provider_gave() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(404).set_body_string(
                r#"{"error":{"message":"No endpoints found for that model.","code":404},"user_id":"u1"}"#,
            ))
            .mount(&server)
            .await;

        let err = post_json(
            &client(),
            HeaderMap::new(),
            format!("{}/chat/completions", server.uri()),
            b"{}".to_vec(),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(&err, ProviderError::Api { status: 404, body } if body == "No endpoints found for that model."),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn an_error_page_is_one_line_and_cut_to_what_a_row_can_hold() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(502).set_body_string(format!(
                "<html>\n<body>{}</body>\n</html>",
                "gateway is down ".repeat(40)
            )))
            .mount(&server)
            .await;

        let err = post_json(
            &client(),
            HeaderMap::new(),
            format!("{}/chat/completions", server.uri()),
            b"{}".to_vec(),
        )
        .await
        .unwrap_err();

        let ProviderError::Api { body, .. } = &err else {
            panic!("expected an api error, got {err}");
        };
        assert!(!body.contains('\n'), "the reason is one line: {body}");
        assert!(body.chars().count() <= MAX_REASON_CHARS + 1, "{body}");
        assert!(body.starts_with("<html> <body>"), "{body}");
    }

    #[tokio::test]
    async fn a_refused_request_without_a_body_names_the_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let err = post_json(
            &client(),
            HeaderMap::new(),
            format!("{}/chat/completions", server.uri()),
            b"{}".to_vec(),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(&err, ProviderError::RateLimit { body, .. } if body == "Too Many Requests"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn a_rate_limit_reads_the_retry_after_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "7")
                    .set_body_string(r#"{"error":{"message":"quota exceeded"}}"#),
            )
            .mount(&server)
            .await;

        let err = post_json(
            &client(),
            HeaderMap::new(),
            format!("{}/chat/completions", server.uri()),
            b"{}".to_vec(),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(&err, ProviderError::RateLimit { retry_after_ms: Some(ms), body } if *ms == 7000 && body == "quota exceeded"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn post_json_reports_malformed_url_instead_of_panicking() {
        let err = post_json(
            &client(),
            HeaderMap::new(),
            MALFORMED_BASE_URL.to_string(),
            b"{}".to_vec(),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, ProviderError::InvalidProviderConfig(_)),
            "unexpected error: {err}"
        );
        assert!(
            err.to_string().contains(MALFORMED_BASE_URL),
            "{err} should quote the url"
        );
    }

    #[tokio::test]
    async fn list_models_reports_malformed_base_url_instead_of_panicking() {
        let provider_name = ProviderName::Custom(PROVIDER.into());
        let err = list_models(
            &client(),
            HeaderMap::new(),
            MALFORMED_BASE_URL,
            &provider_name,
            map_json_err,
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, ProviderError::InvalidProviderConfig(_)),
            "unexpected error: {err}"
        );
        assert!(
            err.to_string().contains(MALFORMED_BASE_URL),
            "{err} should quote the base url"
        );
    }

    #[tokio::test]
    async fn post_json_reports_non_http_scheme_as_config_error() {
        let err = post_json(
            &client(),
            HeaderMap::new(),
            NON_HTTP_BASE_URL.to_string(),
            b"{}".to_vec(),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, ProviderError::InvalidProviderConfig(_)),
            "unexpected error: {err}"
        );
        assert!(
            err.to_string().contains(NON_HTTP_BASE_URL),
            "{err} should quote the url"
        );
    }
}

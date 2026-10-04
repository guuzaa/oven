use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use htmd::HtmlToMarkdown;
use regex::{Captures, Regex};
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, StatusCode, Url};
use serde_json::{Value, json};

use super::{Tool, ToolView, labeled, require_str};
use crate::core::error::AgentError;
use crate::core::turn::TurnContext;

const FORMAT_TEXT: &str = "text";
const FORMAT_MARKDOWN: &str = "markdown";
const SCHEME_HTTP: &str = "http";
const SCHEME_HTTPS: &str = "https";
const USER_AGENT: &str = "oven";
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 10;
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
const HTML_SNIFF_CHARS: usize = 64;
const MEDIA_HTML: &str = "text/html";
const MEDIA_XHTML: &str = "application/xhtml+xml";
const RAW_BLOCK_TAGS: [&str; 4] = ["script", "style", "noscript", "head"];
const ANCHOR_PATTERN: &str =
    r#"(?is)<a\b[^>]*?\bhref\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+))[^>]*>(.*?)</a>"#;
const BLOCK_TAG_PATTERN: &str = r"(?i)<(?:br|/?(?:p|div|h[1-6]|li|tr|table|section|article|header|footer|pre|blockquote|ul|ol|hr|main|figure|figcaption))\b[^>]*>";
const TAG_PATTERN: &str = r"(?is)<[^>]+>";

pub struct WebFetchTool {
    client: Client,
}

impl WebFetchTool {
    pub const NAME: &'static str = "web_fetch";

    pub fn view_input(input: &Value) -> ToolView {
        labeled(Self::NAME, "Fetch", input, "url")
    }

    pub fn new() -> Self {
        Self::default()
    }

    async fn fetch(&self, url: &Url, cx: &TurnContext) -> Result<(String, String), AgentError> {
        let pending = async {
            let response = self.client.get(url.clone()).send().await.map_err(|err| {
                AgentError::from(format!("web_fetch: {}: {err}", public_url(url)))
            })?;
            let status = response.status();
            if !status.is_success() {
                return Err(status_error(url, status));
            }
            let content_type = content_type_of(&response);
            let body = read_body(url, response).await?;
            Ok((content_type, body))
        };
        tokio::select! {
            biased;
            () = cx.cancellation.cancelled() => Err(AgentError::cancelled()),
            result = pending => result,
        }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn view(&self, input: &Value) -> ToolView {
        Self::view_input(input)
    }
    fn description(&self) -> &'static str {
        "Fetch an http(s) URL and return the page for you to read. HTML comes back as \
         markdown by default, keeping headings, lists, and links; set format to \"text\" \
         for plain text. Non-HTML responses are returned unchanged. Nothing is written to disk."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Absolute http or https URL to fetch."
                },
                "format": {
                    "type": "string",
                    "enum": [FORMAT_TEXT, FORMAT_MARKDOWN],
                    "description": "How to return an HTML page. `markdown` (default) keeps structure. `text` is plain text."
                }
            },
            "required": ["url"]
        })
    }
    async fn run(&self, args: &Value, cx: &TurnContext) -> Result<String, AgentError> {
        let url = parse_url(require_str(args, "url", Self::NAME)?)?;
        let format = page_format(args)?;
        let (content_type, body) = self.fetch(&url, cx).await?;
        render(&body, &content_type, format)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageFormat {
    Text,
    Markdown,
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self {
            client: http_client(),
        }
    }
}

fn http_client() -> Client {
    Client::builder()
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
        .user_agent(USER_AGENT)
        .build()
        .expect("web_fetch http client")
}

fn page_format(args: &Value) -> Result<PageFormat, AgentError> {
    let Some(value) = args.get("format") else {
        return Ok(PageFormat::Markdown);
    };
    let Some(raw) = value.as_str() else {
        return Err(AgentError::from("web_fetch: 'format' must be a string"));
    };
    match raw.trim() {
        FORMAT_TEXT => Ok(PageFormat::Text),
        FORMAT_MARKDOWN => Ok(PageFormat::Markdown),
        other => Err(AgentError::from(format!(
            "web_fetch: format must be '{FORMAT_TEXT}' or '{FORMAT_MARKDOWN}', got '{other}'"
        ))),
    }
}

fn parse_url(raw: &str) -> Result<Url, AgentError> {
    let trimmed = raw.trim();
    let url = Url::parse(trimmed)
        .map_err(|_| AgentError::from(format!("web_fetch: invalid url '{trimmed}'")))?;
    if url.scheme() != SCHEME_HTTP && url.scheme() != SCHEME_HTTPS {
        return Err(AgentError::from(format!(
            "web_fetch: unsupported url scheme '{}'",
            url.scheme()
        )));
    }
    Ok(url)
}

fn public_url(url: &Url) -> String {
    let mut clean = url.clone();
    let _ = clean.set_password(None);
    let _ = clean.set_username("");
    clean.to_string()
}

fn status_error(url: &Url, status: StatusCode) -> AgentError {
    AgentError::from(format!("web_fetch: {} returned {status}", public_url(url)))
}

fn content_type_of(response: &reqwest::Response) -> String {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

async fn read_body(url: &Url, response: reqwest::Response) -> Result<String, AgentError> {
    let mut stream = response.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|err| AgentError::from(format!("web_fetch: {}: {err}", public_url(url))))?;
        if buf.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(AgentError::from(format!(
                "web_fetch: {} response exceeds {MAX_BODY_BYTES} bytes",
                public_url(url)
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    String::from_utf8(buf).map_err(|_| {
        AgentError::from(format!(
            "web_fetch: {} response is not valid UTF-8",
            public_url(url)
        ))
    })
}

fn render(body: &str, content_type: &str, format: PageFormat) -> Result<String, AgentError> {
    if !should_convert(content_type, body) {
        return Ok(body.to_string());
    }
    match format {
        PageFormat::Text => Ok(html_to_text(body)),
        PageFormat::Markdown => html_to_markdown(body).map(finish),
    }
}

fn should_convert(content_type: &str, body: &str) -> bool {
    if is_html_type(content_type) {
        return true;
    }
    media_type(content_type).is_empty() && looks_like_html(body)
}

fn is_html_type(content_type: &str) -> bool {
    let media = media_type(content_type);
    media.eq_ignore_ascii_case(MEDIA_HTML) || media.eq_ignore_ascii_case(MEDIA_XHTML)
}

fn media_type(content_type: &str) -> &str {
    content_type.split(';').next().unwrap_or("").trim()
}

fn looks_like_html(body: &str) -> bool {
    let prefix: String = body.trim_start().chars().take(HTML_SNIFF_CHARS).collect();
    let prefix = prefix.to_ascii_lowercase();
    prefix.starts_with("<!doctype html")
        || prefix.starts_with("<html")
        || prefix.starts_with("<head")
        || prefix.starts_with("<body")
}

fn html_to_markdown(html: &str) -> Result<String, AgentError> {
    HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "noscript"])
        .build()
        .convert(html)
        .map_err(|err| AgentError::from(format!("web_fetch: html to markdown: {err}")))
}

fn html_to_text(html: &str) -> String {
    let mut without_hidden = html.to_string();
    for block in raw_blocks() {
        without_hidden = block.replace_all(&without_hidden, "").into_owned();
    }
    let with_links = anchors().replace_all(&without_hidden, |caps: &Captures| anchor_text(caps));
    let with_breaks = block_tags().replace_all(&with_links, "\n");
    let stripped = tags().replace_all(&with_breaks, "");
    normalize_text(&decode_entities(&stripped))
}

fn anchor_text(caps: &Captures) -> String {
    let href = caps
        .get(1)
        .or_else(|| caps.get(2))
        .or_else(|| caps.get(3))
        .map(|value| value.as_str().trim())
        .unwrap_or("");
    let text = collapse_inline(&tags().replace_all(&caps[4], ""));
    if href.is_empty()
        || href.starts_with('#')
        || href.to_ascii_lowercase().starts_with("javascript:")
        || text.is_empty()
        || text == href
    {
        return text;
    }
    format!("{text} ({href})")
}

fn collapse_inline(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_text(text: &str) -> String {
    let mut lines = Vec::new();
    let mut blank = false;
    for line in text.lines() {
        let line = collapse_inline(line);
        if line.is_empty() {
            if !lines.is_empty() && !blank {
                lines.push(String::new());
                blank = true;
            }
            continue;
        }
        blank = false;
        lines.push(line);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.is_empty() {
        String::new()
    } else {
        let mut out = lines.join("\n");
        out.push('\n');
        out
    }
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        if let Some(ch) = decode_entity(&rest[1..end]) {
            out.push(ch);
            rest = &rest[end + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity.to_ascii_lowercase().as_str() {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        _ => decode_numeric(entity),
    }
}

fn decode_numeric(entity: &str) -> Option<char> {
    let (digits, radix) = if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        (hex, 16)
    } else {
        (entity.strip_prefix('#')?, 10)
    };
    let code = u32::from_str_radix(digits, radix).ok()?;
    char::from_u32(code).filter(|ch| !ch.is_control() || *ch == '\n' || *ch == '\t')
}

fn finish(text: String) -> String {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

fn raw_blocks() -> &'static [Regex; 4] {
    static RE: LazyLock<[Regex; 4]> = LazyLock::new(|| {
        RAW_BLOCK_TAGS.map(|tag| {
            Regex::new(&format!(r"(?is)<{tag}\b[^>]*>.*?(?:</{tag}>|$)"))
                .expect("raw block pattern")
        })
    });
    &RE
}

fn anchors() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(ANCHOR_PATTERN).expect("anchor pattern"));
    &RE
}

fn block_tags() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(BLOCK_TAG_PATTERN).expect("block tag pattern"));
    &RE
}

fn tags() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(TAG_PATTERN).expect("tag pattern"));
    &RE
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PAGE_HTML: &str = "<!DOCTYPE html><html><body><h1>Title</h1><p>Hello <a href=\"https://example.com/docs\">docs</a></p><script>secret()</script></body></html>";
    const CONTENT_TYPE_HTML: &str = "text/html; charset=utf-8";
    const NOT_FOUND: &str = "404 Not Found";
    const SCRIPT_BODY: &str = "secret()";
    const UNSUPPORTED_SCHEME: &str = "web_fetch: unsupported url scheme 'file'";
    const MARKDOWN_PAGE: &str = "# Title\n\nHello [docs](https://example.com/docs)\n";
    const TEXT_PAGE: &str = "Title\n\nHello docs (https://example.com/docs)\n";

    fn turn() -> TurnContext {
        TurnContext::for_test()
    }

    async fn serve(status: u16, content_type: &str, body: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(ResponseTemplate::new(status).set_body_raw(body, content_type))
            .mount(&server)
            .await;
        server
    }

    fn page_url(server: &MockServer) -> String {
        format!("{}/page", server.uri())
    }

    #[test]
    fn markdown_is_the_default_format() {
        assert_eq!(page_format(&json!({})).unwrap(), PageFormat::Markdown);
    }

    #[test]
    fn view_names_the_url() {
        let view = WebFetchTool::view_input(&json!({
            "url": "https://example.com/docs",
        }));
        assert_eq!(view.summary, "Fetch https://example.com/docs");
        assert!(view.collapse);
        assert_eq!(
            WebFetchTool::view_input(&json!({})).summary,
            WebFetchTool::NAME
        );
    }

    #[tokio::test]
    async fn returns_html_as_markdown() {
        let server = serve(200, CONTENT_TYPE_HTML, PAGE_HTML).await;
        let tool = WebFetchTool::new();
        let out = tool
            .run(
                &json!({
                    "url": page_url(&server),
                    "format": FORMAT_MARKDOWN,
                }),
                &turn(),
            )
            .await
            .unwrap();
        assert_eq!(out, MARKDOWN_PAGE);
        assert!(!out.contains(SCRIPT_BODY));
        assert!(!out.contains("<h1>"));
    }

    #[tokio::test]
    async fn returns_html_as_plaintext() {
        let server = serve(200, CONTENT_TYPE_HTML, PAGE_HTML).await;
        let tool = WebFetchTool::new();
        let out = tool
            .run(
                &json!({
                    "url": page_url(&server),
                    "format": FORMAT_TEXT,
                }),
                &turn(),
            )
            .await
            .unwrap();
        assert_eq!(out, TEXT_PAGE);
        assert!(!out.contains(SCRIPT_BODY));
        assert!(!out.contains("<h1>"));
        assert_ne!(out, MARKDOWN_PAGE);
    }

    #[tokio::test]
    async fn fetch_failure_returns_an_error() {
        let server = serve(404, CONTENT_TYPE_HTML, "missing").await;
        let tool = WebFetchTool::new();
        let url = page_url(&server);
        let err = tool
            .run(&json!({ "url": url, "format": FORMAT_TEXT }), &turn())
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            format!("web_fetch: {url} returned {NOT_FOUND}")
        );
    }

    #[tokio::test]
    async fn rejects_non_http_url() {
        let tool = WebFetchTool::new();
        let err = tool
            .run(&json!({ "url": "file:///etc/passwd" }), &turn())
            .await
            .unwrap_err();
        assert_eq!(err.message, UNSUPPORTED_SCHEME);
    }
}

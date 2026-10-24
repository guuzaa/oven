//! 录放式回归测试：把录到的一次 HTTP 交互重放一遍。
//!
//! 一个录制文件是一次交互的正文（不含请求头）：
//! - `request`：发给厂商的 wire body
//! - `response`：厂商回的原始响应
//!
//! 重放时用同一份输入重新编码，断言 wire body 与录制的一致；再把录到的响应
//! 喂给解码器，断言得到的是预期的领域对象。encoder 或 decoder 一改，这里就红。
//!
//! fixture 按厂商文档的响应形状手写；拿到真实 key 后可以用录到的响应替换，
//! 测试本身不用改。

use std::path::Path;

use secrecy::SecretString;
use serde::Deserialize;

use super::completions::CompletionsProvider;
use super::completions::decoder::decode_response as decode_completions;
use super::completions::types::ChatCompletionResponse;
use super::responses::ResponsesProvider;
use super::responses::decoder::decode_response as decode_responses;
use super::responses::types::ResponseObject;
use crate::domain::{CallOptions, Message, ModelId, ReasoningEffort, Request};

/// 一条录制：请求正文与响应正文。
#[derive(Debug, Deserialize)]
struct Recording {
    request: serde_json::Value,
    response: serde_json::Value,
}

/// 读取 `yakbak/` 下的录制（测试用）。
///
/// 运行期读取而非 `include_str!` 嵌入，录制变更不需要重编译。
fn load(name: &str) -> Recording {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/provider/yakbak")
        .join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read yakbak recording {path:?}: {err}"));
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("failed to parse yakbak recording {path:?}: {err}"))
}

fn api_key(value: &str) -> SecretString {
    SecretString::new(value.to_string().into())
}

fn user_request(model: &str, prompt: &str) -> Request {
    Request {
        model: ModelId::from(model),
        messages: vec![Message::user_text(prompt)],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kimi_k3_takes_the_level_and_reads_the_thinking_back() {
        let recording = load("kimi_k3.json");
        let provider = CompletionsProvider::moonshot(api_key("k"));
        let request = user_request("kimi-k3", "求 1+1。");
        let options = CallOptions::default().reasoning_effort(ReasoningEffort::High);

        let body = provider.build_body(&request, &options, false).unwrap();
        assert_eq!(body, recording.request);

        let wire: ChatCompletionResponse = serde_json::from_value(recording.response).unwrap();
        let response = decode_completions(wire).unwrap();
        assert_eq!(response.thinking(), "用户问的是加法，结果是 2。");
        assert_eq!(response.text(), "1+1=2。");
    }

    #[test]
    fn kimi_k2_6_takes_the_switch_and_thinks_nothing() {
        let recording = load("kimi_k2_6.json");
        let provider = CompletionsProvider::moonshot(api_key("k"));
        let request = user_request("kimi-k2.6", "求 1+1。");
        let options = CallOptions::default().reasoning_effort(ReasoningEffort::Off);

        let body = provider.build_body(&request, &options, false).unwrap();
        assert_eq!(body, recording.request);

        let wire: ChatCompletionResponse = serde_json::from_value(recording.response).unwrap();
        let response = decode_completions(wire).unwrap();
        assert_eq!(response.thinking(), "");
        assert_eq!(response.text(), "1+1=2。");
    }

    #[test]
    fn minimax_splits_the_thinking_out_and_reads_it_back() {
        let recording = load("minimax_m3.json");
        let provider = CompletionsProvider::minimax(api_key("k"));
        let request = user_request("MiniMax-M3", "求 1+1。");
        let options = CallOptions::default().reasoning_effort(ReasoningEffort::High);

        let body = provider.build_body(&request, &options, false).unwrap();
        assert_eq!(body, recording.request);

        let wire: ChatCompletionResponse = serde_json::from_value(recording.response).unwrap();
        let response = decode_completions(wire).unwrap();
        assert_eq!(response.thinking(), "用户问的是加法，结果是 2。");
        assert_eq!(response.text(), "1+1=2。");
    }

    #[test]
    fn grok_takes_the_level_and_reads_the_summary_back() {
        let recording = load("grok_4_6.json");
        let provider = ResponsesProvider::grok(api_key("k"));
        let request = user_request("grok-4.6", "求 1+1。");
        let options = CallOptions::default().reasoning_effort(ReasoningEffort::XHigh);

        let body = provider.build_body(&request, &options, false).unwrap();
        assert_eq!(body, recording.request);

        let wire: ResponseObject = serde_json::from_value(recording.response).unwrap();
        let response = decode_responses(wire).unwrap();
        assert_eq!(response.thinking(), "用户问的是加法，结果是 2。");
        assert_eq!(response.text(), "1+1=2。");
        assert_eq!(response.usage.map(|usage| usage.reasoning_tokens), Some(9));
    }
}

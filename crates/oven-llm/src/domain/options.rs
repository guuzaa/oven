//! `CallOptions`：一次调用的可调项。
//!
//! 与 [`Request`](super::request::Request) 分开：请求是一次对话（说给哪个
//! 模型听），选项是这次怎么问。同一个请求可以配不同的选项发出去。

use serde::{Deserialize, Serialize};

use super::reasoning::ReasoningEffort;

/// 采样参数：控制模型生成时的随机性与长度限制。
///
/// 默认值一律是"不带"：厂商的默认值就是对的，而 Kimi 一族的 temperature 是
/// 按模型固定的值（思考 1.0、非思考 0.6），显式传入别的值会报错。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SamplingParams {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stop: Option<Vec<String>>,
}

/// 一次调用的可调项。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CallOptions {
    /// 采样参数。
    pub sampling: SamplingParams,
    /// 推理强度。`None` 表示不表态：不下发任何推理字段，交给上游默认。
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Provider 私有参数直通（跳过标准 Serialize，由 transport 层 merge 进
    /// wire JSON body，键名需与 provider wire format 一致，不应与标准字段重名）。
    #[serde(skip, default)]
    pub provider_options: serde_json::Map<String, serde_json::Value>,
}

impl CallOptions {
    /// 设置 temperature。
    pub fn temperature(mut self, value: f32) -> Self {
        self.sampling.temperature = Some(value);
        self
    }

    /// 设置 top_p。
    pub fn top_p(mut self, value: f32) -> Self {
        self.sampling.top_p = Some(value);
        self
    }

    /// 设置 max_tokens。
    pub fn max_tokens(mut self, value: u32) -> Self {
        self.sampling.max_tokens = Some(value);
        self
    }

    /// 设置 stop 序列。
    pub fn stop(mut self, sequences: Vec<String>) -> Self {
        self.sampling.stop = Some(sequences);
        self
    }

    /// 设置推理强度。未设置表示不表态：不下发任何推理字段。
    pub fn reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// 添加一个 provider 私有参数。
    pub fn provider_option(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.provider_options.insert(key.into(), value);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setters_fill_what_they_say() {
        let options = CallOptions::default()
            .temperature(0.7)
            .top_p(0.9)
            .max_tokens(1024)
            .stop(vec!["STOP".to_string()])
            .reasoning_effort(ReasoningEffort::High)
            .provider_option("top_k", serde_json::json!(40));

        assert_eq!(options.sampling.temperature, Some(0.7));
        assert_eq!(options.sampling.top_p, Some(0.9));
        assert_eq!(options.sampling.max_tokens, Some(1024));
        assert_eq!(
            options.sampling.stop.as_ref().unwrap(),
            &["STOP".to_string()]
        );
        assert_eq!(options.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(options.provider_options["top_k"], 40);
    }

    #[test]
    fn the_default_options_say_nothing() {
        let options = CallOptions::default();
        assert_eq!(options.reasoning_effort, None);
        assert!(options.provider_options.is_empty());
        assert_eq!(options.sampling.temperature, None);
        assert_eq!(options.sampling, SamplingParams::default());
    }
}

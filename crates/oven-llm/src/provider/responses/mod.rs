//! OpenAI Responses API Provider 实现：wire 类型、encoder、decoder、静态
//! 模型元数据与 `Provider` 实现。
//!
//! 结构镜像 `openai_compat`，但 wire 格式为 OpenAI Responses API（`POST
//! /responses` + SSE 事件流），内置 DeepSeek / Grok（xAI）/ OpenAI 官方三个
//! 预设，并保留 `with_base_url` / `with_models` 自定义入口。

pub mod decoder;
pub mod encoder;
pub mod models;
pub mod provider;
#[cfg(test)]
mod testdata;
pub mod types;

pub use decoder::DecodeError;
pub use encoder::EncodeError;
pub use provider::ResponsesProvider;

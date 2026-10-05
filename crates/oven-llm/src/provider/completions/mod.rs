//! OpenAI 兼容 Provider 实现：wire 类型、encoder、decoder、静态模型元数据
//! 与 `Provider` 实现。

pub mod decoder;
pub mod encoder;
pub mod models;
pub mod provider;
pub mod types;

pub use decoder::CompletionsDecodeError;
pub use encoder::CompletionsEncodeError;
pub use provider::CompletionsProvider;

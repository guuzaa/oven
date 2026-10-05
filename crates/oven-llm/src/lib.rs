//! `oven-llm` 核心能力层：统一的多 provider LLM 调用抽象。

mod domain;
mod provider;

pub use domain::*;
pub use provider::*;

/// 密钥包装类型（`secrecy` crate 的 `SecretString`），供需要显式包装 API key
/// 的调用方使用；普通调用方直接传 `String` / `&str` 即可，无需依赖 `secrecy`。
pub use secrecy::SecretString;

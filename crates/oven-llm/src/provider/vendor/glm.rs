//! Zhipu（GLM）系：只有 `thinking.type` 开关，没有档位。
//!
//! 档位集取自 `pro.md` 的厂商调查（§3.1）：GLM 只有开/关。给 GLM 发
//! `reasoning_effort` 会被静默忽略，所以这里不发。

use super::Dialect;
use super::capability::EffortSupport;

/// Zhipu 系的推理方言。
pub const DIALECT: Dialect = Dialect::Toggle;

/// Zhipu 系模型支持的档位：只有开关。
pub const CAPABILITIES: EffortSupport = EffortSupport::Toggle;

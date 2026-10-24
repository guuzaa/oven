//! xAI（Grok）系：档位走 `reasoning.effort`（Responses 协议）。
//!
//! - `grok-4.5` / `grok-4.6` / `grok-4.7`：支持 `low` / `medium` / `high`
//!   / `xhigh`，默认 `high`。
//! - `grok-4.3`：多一档 `none`，默认 `low`。
//! - `grok-build-0.1` 与 `grok-4.20-*-reasoning`：会推理，但没有档位可选
//!   ——深度固定，请求里不该带推理参数。
//! - `grok-4.20-*-non-reasoning`：不推理。

use std::borrow::Cow;

use super::Dialect;
use super::capability::EffortSupport;
use crate::domain::ReasoningEffort;

/// xAI 系的推理方言。
pub const DIALECT: Dialect = Dialect::Level;

/// xAI 系模型支持的档位。
pub const CAPABILITIES: EffortSupport = EffortSupport::Levels(Cow::Borrowed(&[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
]));

/// 会推理但深度固定的模型（如 `grok-build-0.1`）：请求里不该带推理参数。
pub const FIXED_DEPTH: EffortSupport = EffortSupport::Fixed;

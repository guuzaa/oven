//! Moonshot（Kimi）系：档位走 `reasoning_effort`，开关走 `thinking.type`，
//! 两者**按模型分开**。
//!
//! - `kimi-k3` / `kimi-k3.1-pro`：思考强度用顶层 `reasoning_effort`
//!   （`low` / `high` / `max`，默认 `max`），不支持 `thinking` 参数。
//! - `kimi-k2.6`：默认就思考，用 `thinking.type`（`enabled` / `disabled`）
//!   控制，不支持 `reasoning_effort`。
//! - `kimi-k2.7-code`：始终思考，`thinking.type` 只接受 `enabled`
//!   （传 `disabled` 报错），也不支持 `reasoning_effort`。
//!
//! 思考内容在 `reasoning_content` 里回来，多轮要把它原样带回去。

use std::borrow::Cow;

use super::Dialect;
use super::capability::EffortSupport;
use crate::domain::ReasoningEffort;

/// Moonshot 系的推理方言：K3 一代走档位通道。
pub const DIALECT: Dialect = Dialect::Level;

/// Moonshot 系默认的档位集：K3 一代的 `low` / `high` / `max`（没有 `medium`）。
pub const CAPABILITIES: EffortSupport = EffortSupport::Levels(Cow::Borrowed(&[
    ReasoningEffort::Low,
    ReasoningEffort::High,
    ReasoningEffort::Max,
]));

/// `kimi-k2.6`：只有开关，默认就思考。
pub const K2_6: EffortSupport = EffortSupport::Toggle;

/// `kimi-k2.7-code`：始终思考，既不能调深度也不能关。
pub const K2_7_CODE: EffortSupport = EffortSupport::Fixed;

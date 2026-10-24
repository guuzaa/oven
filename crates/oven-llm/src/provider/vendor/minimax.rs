//! MiniMax 系：思考深度走 `reasoning_effort`，思考开关走 `thinking.type`，
//! 两者**按模型分开**。
//!
//! - `MiniMax-M3.1-Flash` 一类：思考始终开启，用 `reasoning_effort`
//!   （`low`/`medium`/`high`/`xhigh`/`max`）调深度。传
//!   `thinking: {"type":"disabled"}` 或 `reasoning_effort: "none"` 会 400。
//! - `MiniMax-M3` 一类：默认自适应思考，用 `thinking: {"type":"disabled"}`
//!   关闭，不接受 `reasoning_effort`。
//!
//! 思考内容在 `reasoning_split: true` 时单独回到 `reasoning_content`；不
//! 打开就在 `content` 里以标签返回。因此这个厂商每次请求都带上它。

use std::borrow::Cow;

use super::Dialect;
use super::capability::EffortSupport;
use crate::domain::ReasoningEffort;

/// MiniMax 系的推理方言：两条通道都在，用哪一条由模型的能力决定。
pub const DIALECT: Dialect = Dialect::Both;

/// MiniMax 系里"思考始终开启、用深度调档位"的那一类模型支持的档位。
pub const CAPABILITIES: EffortSupport = EffortSupport::Levels(Cow::Borrowed(&[
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
    ReasoningEffort::Max,
]));

/// 该厂商每次请求都要带的额外字段：思考内容单独回到 `reasoning_content`。
pub fn extra_fields() -> Vec<(&'static str, serde_json::Value)> {
    vec![("reasoning_split", serde_json::Value::Bool(true))]
}

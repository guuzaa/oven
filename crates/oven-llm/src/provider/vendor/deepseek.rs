//! DeepSeek 系：档位走 `reasoning_effort`，档位集**不连续**。
//!
//! 档位集取自 `pro.md` 的厂商调查（§3.1）：`none` / `low` / `high` / `max`
//! —— 没有 `medium`。所以钳制要按强度比大小取档，不能按索引取。

use std::borrow::Cow;

use super::Dialect;
use super::capability::EffortSupport;
use crate::domain::ReasoningEffort;

/// DeepSeek 系的推理方言。
pub const DIALECT: Dialect = Dialect::Level;

/// DeepSeek 系模型支持的档位。
pub const CAPABILITIES: EffortSupport = EffortSupport::Levels(Cow::Borrowed(&[
    ReasoningEffort::Off,
    ReasoningEffort::Low,
    ReasoningEffort::High,
    ReasoningEffort::Max,
]));

//! OpenAI 系：档位走 `reasoning_effort`（Completions）/ `reasoning.effort`
//! （Responses）。
//!
//! 档位集取自 `pro.md` 的厂商调查（§3.1）：GPT-5 一代出现过比 `low` 更弱的
//! `minimal`。厂商改了档位就改这里。

use std::borrow::Cow;

use super::Dialect;
use super::capability::EffortSupport;
use crate::domain::ReasoningEffort;

/// OpenAI 系的推理方言。
pub const DIALECT: Dialect = Dialect::Level;

/// OpenAI 系模型支持的档位。
pub const CAPABILITIES: EffortSupport = EffortSupport::Levels(Cow::Borrowed(&[
    ReasoningEffort::Minimal,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
]));

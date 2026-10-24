//! 模型对推理的支持程度。
//!
//! 这是**模型级**的事实：这个模型能表达哪些档位。厂商方言（wire 上用哪条
//! 通道说话）在 [`Dialect`](super::Dialect) 里，两者不要混。

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::domain::ReasoningEffort;

/// 模型对推理的支持程度。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum EffortSupport {
    /// 不知道：不干预，按方言的默认行为发送。
    #[default]
    Unknown,
    /// 完全不支持推理：不发任何推理字段。
    Unsupported,
    /// 会推理，但深度固定：既不发档位也不发开关，由模型自己决定。
    Fixed,
    /// 只有开关，没有档位：任意档位都表达"开启"，`Off` 表达"关闭"。
    Toggle,
    /// 有档位：这些档位可用（升序、去重、非空）。不含 `Off` 表示不支持
    /// 显式关闭。
    Levels(Cow<'static, [ReasoningEffort]>),
}

impl EffortSupport {
    /// 该模型可用的档位。`Toggle` 没有档位，`Unknown` 不干预。
    pub fn levels(&self) -> &[ReasoningEffort] {
        match self {
            Self::Levels(levels) => levels,
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_has_no_levels() {
        assert!(EffortSupport::default().levels().is_empty());
        assert!(EffortSupport::Toggle.levels().is_empty());
        assert!(EffortSupport::Unsupported.levels().is_empty());
    }

    #[test]
    fn levels_expose_the_declared_set() {
        let support = EffortSupport::Levels(Cow::Borrowed(&[
            ReasoningEffort::Low,
            ReasoningEffort::High,
        ]));
        assert_eq!(
            support.levels(),
            &[ReasoningEffort::Low, ReasoningEffort::High]
        );
    }
}

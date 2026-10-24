//! `ReasoningEffort`：推理强度词表。
//!
//! 词表只有一根轴：**意图**。用户要什么由调用方决定，这个模型支持什么由
//! provider 层的能力决定，两者在此处不混。

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// 强度阶梯，升序。不含 `Off`（关闭不是档位）与 `Budget`（预算不在阶梯上）。
pub const LADDER: [ReasoningEffort; 6] = [
    ReasoningEffort::Minimal,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
    ReasoningEffort::Max,
];

/// 推理强度：控制模型在 thinking 阶段投入的计算量。
///
/// 未设置（`Option::None`）表示"不表态"：不下发任何推理字段，交给上游默认。
/// [`Off`](Self::Off) 表示显式关闭。两者语义不同，不可互相替代。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    /// 显式关闭推理。
    Off,
    /// 比 `Low` 更弱的档位。
    Minimal,
    Low,
    Medium,
    High,
    /// 比 `High` 更强的档位。
    XHigh,
    /// 最强档位。
    Max,
    /// 厂商按 token 预算表达推理强度。
    Budget(u32),
}

impl ReasoningEffort {
    /// 规范档位词。预算类档位没有档位词，返回 `None`。
    ///
    /// 这是各厂商共用的拼写；厂商特有的拼写由 provider 层的 vendor 映射处理。
    pub const fn keyword(self) -> Option<&'static str> {
        match self {
            Self::Off => Some("none"),
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::XHigh => Some("xhigh"),
            Self::Max => Some("max"),
            Self::Budget(_) => None,
        }
    }

    /// 强度阶梯上的位置。`Off` 与 `Budget` 不在阶梯上。
    pub const fn ladder_index(self) -> Option<usize> {
        match self {
            Self::Minimal => Some(0),
            Self::Low => Some(1),
            Self::Medium => Some(2),
            Self::High => Some(3),
            Self::XHigh => Some(4),
            Self::Max => Some(5),
            Self::Off | Self::Budget(_) => None,
        }
    }

    /// 是否显式关闭推理。
    pub const fn is_off(self) -> bool {
        matches!(self, Self::Off)
    }

    /// 厂商按预算表达时的 token 数。非预算档位返回 `None`。
    pub const fn budget_tokens(self) -> Option<u32> {
        match self {
            Self::Budget(tokens) => Some(tokens),
            _ => None,
        }
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Budget(tokens) => write!(formatter, "{tokens}"),
            other => formatter.write_str(other.keyword().unwrap_or_default()),
        }
    }
}

impl FromStr for ReasoningEffort {
    type Err = ParseEffortError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "zero" => Ok(Self::Off),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" | "x-high" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            other => other
                .parse::<u32>()
                .map(Self::Budget)
                .map_err(|_| ParseEffortError {
                    actual: raw.to_string(),
                }),
        }
    }
}

impl Serialize for ReasoningEffort {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ReasoningEffort {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// 推理强度解析失败。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "invalid reasoning effort '{actual}'; expected none, minimal, low, medium, high, xhigh, max, or a token budget"
)]
pub struct ParseEffortError {
    /// 无法解析的原始输入。
    pub actual: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_EFFORTS: [ReasoningEffort; 8] = [
        ReasoningEffort::Off,
        ReasoningEffort::Minimal,
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::XHigh,
        ReasoningEffort::Max,
        ReasoningEffort::Budget(2048),
    ];

    #[test]
    fn display_uses_the_wire_spelling() {
        let words = [
            (ReasoningEffort::Off, "none"),
            (ReasoningEffort::Minimal, "minimal"),
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::XHigh, "xhigh"),
            (ReasoningEffort::Max, "max"),
            (ReasoningEffort::Budget(2048), "2048"),
        ];
        for (effort, word) in words {
            assert_eq!(effort.to_string(), word);
        }
    }

    #[test]
    fn parse_accepts_canonical_words_aliases_and_budgets() {
        assert_eq!("none".parse(), Ok(ReasoningEffort::Off));
        assert_eq!("off".parse(), Ok(ReasoningEffort::Off));
        assert_eq!("zero".parse(), Ok(ReasoningEffort::Off));
        assert_eq!("NONE".parse(), Ok(ReasoningEffort::Off));
        assert_eq!(" minimal ".parse(), Ok(ReasoningEffort::Minimal));
        assert_eq!("xhigh".parse(), Ok(ReasoningEffort::XHigh));
        assert_eq!("x-high".parse(), Ok(ReasoningEffort::XHigh));
        assert_eq!("2048".parse(), Ok(ReasoningEffort::Budget(2048)));
    }

    #[test]
    fn parse_rejects_unknown_words() {
        let error = "hih".parse::<ReasoningEffort>().unwrap_err();
        assert_eq!(error.actual, "hih");
        assert_eq!(
            error.to_string(),
            "invalid reasoning effort 'hih'; expected none, minimal, low, medium, high, xhigh, max, or a token budget"
        );
    }

    #[test]
    fn round_trips_through_serde_as_a_plain_string() {
        for effort in ALL_EFFORTS {
            let json = serde_json::to_value(effort).unwrap();
            assert_eq!(json, serde_json::Value::String(effort.to_string()));
            let decoded: ReasoningEffort = serde_json::from_value(json).unwrap();
            assert_eq!(decoded, effort);
        }
    }

    #[test]
    fn ladder_is_ascending_and_matches_ladder_index() {
        for (index, effort) in LADDER.iter().enumerate() {
            assert_eq!(effort.ladder_index(), Some(index));
        }
        assert!(ReasoningEffort::Off.ladder_index().is_none());
        assert!(ReasoningEffort::Budget(2048).ladder_index().is_none());
    }

    #[test]
    fn ladder_is_strictly_ascending() {
        for window in LADDER.windows(2) {
            let (lower, higher) = (window[0], window[1]);
            assert!(lower.ladder_index() < higher.ladder_index());
        }
    }

    #[test]
    fn off_and_budget_are_recognized() {
        assert!(ReasoningEffort::Off.is_off());
        assert!(!ReasoningEffort::Low.is_off());
        assert_eq!(ReasoningEffort::Budget(512).budget_tokens(), Some(512));
        assert_eq!(ReasoningEffort::High.budget_tokens(), None);
    }

    #[test]
    fn budget_has_no_keyword() {
        assert_eq!(ReasoningEffort::Budget(2048).keyword(), None);
        assert_eq!(ReasoningEffort::Off.keyword(), Some("none"));
        assert_eq!(ReasoningEffort::High.keyword(), Some("high"));
    }
}

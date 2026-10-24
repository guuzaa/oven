//! 归一化：把请求里的推理意图按模型能力收敛成 wire 上能说的话。
//!
//! 规则只有一处在这里：意图（用户要什么）加能力（模型能表达什么）得出
//! 归一化意图，再按厂商方言拆到各通道上。只向下、永不报错、不报告降级。

use crate::domain::ReasoningEffort;

use super::Dialect;
use super::capability::EffortSupport;

/// 归一化后的推理意图：与 wire 无关，只说"关还是开、开到什么程度"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningIntent {
    /// 不表态：不发任何推理字段，交给上游默认。
    Unset,
    /// 关闭推理。
    Off,
    /// 开启推理，不指定档位。
    On,
    /// 开启推理，指定档位。
    At(ReasoningEffort),
}

/// 各通道上最终要发的值。`None` = 该通道不发。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Channels {
    /// 档位通道上的词（`reasoning_effort` / `reasoning.effort`）。
    pub level: Option<&'static str>,
    /// 开关通道上的值（`thinking.type`）。
    pub toggle: Option<bool>,
}

/// 把请求意图按模型能力归一化。
///
/// - `Unknown`：不干预，原样通过。
/// - `Unsupported`：不发任何推理字段。
/// - `Toggle`：档位请求折叠成"开启"，关闭保持关闭。
/// - `Levels(S)`：档位按强度钳到 `S` 里不强于请求的最高档；请求比 `S` 里
///   最弱的还弱时取 `S` 里最弱的档；`S` 不含 `Off` 时"关闭"降到最弱的档。
pub fn resolve_reasoning(
    intent: Option<ReasoningEffort>,
    support: &EffortSupport,
) -> ReasoningIntent {
    let Some(requested) = intent else {
        return ReasoningIntent::Unset;
    };
    match support {
        EffortSupport::Unknown => match requested {
            ReasoningEffort::Off => ReasoningIntent::Off,
            other => ReasoningIntent::At(other),
        },
        EffortSupport::Unsupported => ReasoningIntent::Unset,
        EffortSupport::Toggle => match requested {
            ReasoningEffort::Off => ReasoningIntent::Off,
            _ => ReasoningIntent::On,
        },
        EffortSupport::Levels(supported) => match requested {
            ReasoningEffort::Off if supported.contains(&ReasoningEffort::Off) => {
                ReasoningIntent::Off
            }
            other => match clamp_level(other, supported) {
                Some(level) => ReasoningIntent::At(level),
                None => ReasoningIntent::Unset,
            },
        },
    }
}

/// 按方言把归一化意图拆到各通道上。
pub fn channels(intent: ReasoningIntent, dialect: Dialect) -> Channels {
    let level = match intent {
        ReasoningIntent::Unset | ReasoningIntent::On => None,
        ReasoningIntent::Off => Some("none"),
        ReasoningIntent::At(effort) => effort.keyword(),
    };
    let toggle = match intent {
        ReasoningIntent::Unset => None,
        ReasoningIntent::Off => Some(false),
        ReasoningIntent::On | ReasoningIntent::At(_) => Some(true),
    };
    match dialect {
        Dialect::Level => Channels {
            level,
            toggle: None,
        },
        Dialect::Toggle => Channels {
            level: None,
            toggle,
        },
        Dialect::Both => Channels { level, toggle },
    }
}

/// 把档位钳到模型支持的集合里。
///
/// 取不强于请求的最高档；请求（`Off` 或预算）不在强度阶梯上时，取集合里
/// 最弱的档。集合里没有可用档位时返回 `None`。
fn clamp_level(
    requested: ReasoningEffort,
    supported: &[ReasoningEffort],
) -> Option<ReasoningEffort> {
    match requested.ladder_index() {
        Some(requested_rank) => supported
            .iter()
            .copied()
            .filter(|level| {
                level
                    .ladder_index()
                    .is_some_and(|rank| rank <= requested_rank)
            })
            .max_by_key(|level| level.ladder_index())
            .or_else(|| weakest_level(supported)),
        None => weakest_level(supported),
    }
}

/// 集合里最弱的档。`Off` 不是档位，不参与。
fn weakest_level(supported: &[ReasoningEffort]) -> Option<ReasoningEffort> {
    supported
        .iter()
        .copied()
        .filter(|level| level.ladder_index().is_some())
        .min_by_key(|level| level.ladder_index())
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    fn levels(levels: &[ReasoningEffort]) -> EffortSupport {
        EffortSupport::Levels(Cow::Owned(levels.to_vec()))
    }

    #[test]
    fn an_unset_intent_stays_unset_whatever_the_model_supports() {
        for support in [
            EffortSupport::Unknown,
            EffortSupport::Unsupported,
            EffortSupport::Toggle,
            levels(&[ReasoningEffort::Low]),
        ] {
            assert_eq!(resolve_reasoning(None, &support), ReasoningIntent::Unset);
        }
    }

    #[test]
    fn an_unsupported_model_sends_nothing() {
        assert_eq!(
            resolve_reasoning(Some(ReasoningEffort::High), &EffortSupport::Unsupported),
            ReasoningIntent::Unset
        );
        assert_eq!(
            resolve_reasoning(Some(ReasoningEffort::Off), &EffortSupport::Unsupported),
            ReasoningIntent::Unset
        );
    }

    #[test]
    fn an_unknown_model_passes_the_intent_through() {
        assert_eq!(
            resolve_reasoning(Some(ReasoningEffort::High), &EffortSupport::Unknown),
            ReasoningIntent::At(ReasoningEffort::High)
        );
        assert_eq!(
            resolve_reasoning(Some(ReasoningEffort::Off), &EffortSupport::Unknown),
            ReasoningIntent::Off
        );
    }

    #[test]
    fn a_toggle_only_model_folds_a_level_into_on() {
        assert_eq!(
            resolve_reasoning(Some(ReasoningEffort::High), &EffortSupport::Toggle),
            ReasoningIntent::On
        );
        assert_eq!(
            resolve_reasoning(Some(ReasoningEffort::Off), &EffortSupport::Toggle),
            ReasoningIntent::Off
        );
    }

    #[test]
    fn a_level_is_clamped_down_to_the_supported_set() {
        let support = levels(&[ReasoningEffort::Low, ReasoningEffort::High]);
        assert_eq!(
            resolve_reasoning(Some(ReasoningEffort::Medium), &support),
            ReasoningIntent::At(ReasoningEffort::Low)
        );
        assert_eq!(
            resolve_reasoning(Some(ReasoningEffort::Max), &support),
            ReasoningIntent::At(ReasoningEffort::High)
        );
        assert_eq!(
            resolve_reasoning(Some(ReasoningEffort::High), &support),
            ReasoningIntent::At(ReasoningEffort::High)
        );
    }

    #[test]
    fn a_level_weaker_than_the_whole_set_falls_to_the_weakest() {
        let support = levels(&[ReasoningEffort::Medium, ReasoningEffort::High]);
        assert_eq!(
            resolve_reasoning(Some(ReasoningEffort::Low), &support),
            ReasoningIntent::At(ReasoningEffort::Medium)
        );
    }

    #[test]
    fn a_set_without_off_lowers_the_request_to_the_weakest_level() {
        let support = levels(&[ReasoningEffort::Low, ReasoningEffort::High]);
        assert_eq!(
            resolve_reasoning(Some(ReasoningEffort::Off), &support),
            ReasoningIntent::At(ReasoningEffort::Low)
        );
    }

    #[test]
    fn a_set_with_off_keeps_the_request_off() {
        let support = levels(&[ReasoningEffort::Off, ReasoningEffort::Low]);
        assert_eq!(
            resolve_reasoning(Some(ReasoningEffort::Off), &support),
            ReasoningIntent::Off
        );
    }

    #[test]
    fn a_budget_falls_to_the_weakest_level() {
        let support = levels(&[ReasoningEffort::Low, ReasoningEffort::High]);
        assert_eq!(
            resolve_reasoning(Some(ReasoningEffort::Budget(2048)), &support),
            ReasoningIntent::At(ReasoningEffort::Low)
        );
    }

    #[test]
    fn the_level_dialect_fills_only_the_level_channel() {
        let channels = channels(ReasoningIntent::At(ReasoningEffort::High), Dialect::Level);
        assert_eq!(channels.level, Some("high"));
        assert_eq!(channels.toggle, None);
    }

    #[test]
    fn the_toggle_dialect_fills_only_the_toggle_channel() {
        let channels = channels(ReasoningIntent::At(ReasoningEffort::High), Dialect::Toggle);
        assert_eq!(channels.level, None);
        assert_eq!(channels.toggle, Some(true));
    }

    #[test]
    fn the_permissive_dialect_fills_both_channels() {
        let channels = channels(ReasoningIntent::At(ReasoningEffort::High), Dialect::Both);
        assert_eq!(channels.level, Some("high"));
        assert_eq!(channels.toggle, Some(true));
    }

    #[test]
    fn off_fills_the_level_channel_with_none_and_the_toggle_with_false() {
        let channels = channels(ReasoningIntent::Off, Dialect::Both);
        assert_eq!(channels.level, Some("none"));
        assert_eq!(channels.toggle, Some(false));
    }

    #[test]
    fn on_fills_the_toggle_channel_only() {
        let channels = channels(ReasoningIntent::On, Dialect::Both);
        assert_eq!(channels.level, None);
        assert_eq!(channels.toggle, Some(true));
    }

    #[test]
    fn a_budget_has_no_level_word() {
        let channels = channels(
            ReasoningIntent::At(ReasoningEffort::Budget(2048)),
            Dialect::Both,
        );
        assert_eq!(channels.level, None);
        assert_eq!(channels.toggle, Some(true));
    }

    /// 意图 × 能力 × 方言：每个组合都过一遍，三条不变量全程成立。
    #[test]
    fn the_matrix_pins_every_combination() {
        let supports = [
            ("unknown", EffortSupport::Unknown),
            ("unsupported", EffortSupport::Unsupported),
            ("toggle", EffortSupport::Toggle),
            (
                "levels",
                levels(&[
                    ReasoningEffort::Off,
                    ReasoningEffort::Low,
                    ReasoningEffort::High,
                ]),
            ),
        ];
        let intents = [
            ("unset", None),
            ("off", Some(ReasoningEffort::Off)),
            ("low", Some(ReasoningEffort::Low)),
            ("high", Some(ReasoningEffort::High)),
            ("max", Some(ReasoningEffort::Max)),
            ("budget", Some(ReasoningEffort::Budget(2048))),
        ];
        let dialects = [
            ("level", Dialect::Level),
            ("toggle", Dialect::Toggle),
            ("both", Dialect::Both),
        ];

        for (support_name, support) in &supports {
            for (intent_name, intent) in &intents {
                let resolved = resolve_reasoning(*intent, support);
                for (dialect_name, dialect) in &dialects {
                    let channels = channels(resolved, *dialect);
                    let label = format!("{support_name} × {intent_name} × {dialect_name}");

                    // 不表态与不支持都不发任何字段。
                    if matches!(resolved, ReasoningIntent::Unset) {
                        assert_eq!(channels, Channels::default(), "{label}");
                    }

                    // 档位通道上的词要么是"关闭"，要么在模型支持的集合里；能力未知的模型
                    // 不干预，原样通过。
                    if let Some(word) = channels.level {
                        let allowed = word == "none"
                            || matches!(support, EffortSupport::Unknown)
                            || support
                                .levels()
                                .iter()
                                .any(|level| level.keyword() == Some(word));
                        assert!(allowed, "{label}: {word}");
                    }

                    // 开关关掉时，档位通道要么说"关闭"，要么不说话。
                    if channels.toggle == Some(false) {
                        assert!(
                            matches!(channels.level, None | Some("none")),
                            "{label}: {:?}",
                            channels.level
                        );
                    }
                }
            }
        }
    }
}

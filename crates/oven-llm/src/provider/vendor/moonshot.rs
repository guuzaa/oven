//! Moonshot（Kimi）系：档位走 `reasoning_effort`。
//!
//! 档位集尚未核实，先留 `Unknown`（不干预，按方言的默认行为发送）。核实后
//! 改成 `Levels`。
//!
//! 方言同样未核实：Kimi 读不读 `thinking.type` 没有定论，所以两条通道都发
//! （与今天一致），核实后再收窄成 `Level`。

use super::Dialect;
use super::capability::EffortSupport;

/// Moonshot 系的推理方言。
pub const DIALECT: Dialect = Dialect::Both;

/// Moonshot 系模型支持的档位：尚未核实。
pub const CAPABILITIES: EffortSupport = EffortSupport::Unknown;

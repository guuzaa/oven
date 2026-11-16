//! Tidy: one model call that turns the inbox and the curated layers into a
//! change set, and the code that applies it under the rules the model does
//! not get to bend.

mod apply;
mod input;
mod log;
mod plan;

pub use apply::{Change, TidyContext, TidyReport, apply_plan};
pub use input::{Elsewhere, TidyInput, TidyMemory, render_input};
pub use plan::{PutOp, TidyOp, TidyPlan, parse_plan};

pub const TIDY_PROMPT: &str = include_str!("../tidy_prompt.md");

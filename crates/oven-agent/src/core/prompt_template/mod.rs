mod instructions;
mod mode;
mod subagent;
mod system;
mod top_level;

pub use instructions::{InstructionDoc, InstructionScope, load_instructions};
pub use subagent::subagent_preamble;
pub use system::{MEMORY_PROMPT, system_prompt};
pub use top_level::TOP_LEVEL_PROMPT;

pub(crate) use mode::{PLAN_REMINDER, PLAN_REMINDER_AFTER_MISSES, compose_system};

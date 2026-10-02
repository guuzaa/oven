use super::{CommandContext, CommandOutcome, SlashCommand};
use crate::core::error::AppError;
use crate::memory;

pub struct Memory;

impl SlashCommand for Memory {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn description(&self) -> &'static str {
        "List, show or remove memories: /memory [show <ref> | rm <ref>]"
    }

    fn execute(&self, cx: &mut CommandContext<'_>, args: &str) -> Result<CommandOutcome, AppError> {
        let action = memory::parse_command(args)?;
        // The store lives on the runtime, so this asks for the driver: without
        // one the command waits until the running turn lets go of it.
        cx.agent()?;
        Ok(CommandOutcome::Memory(action))
    }
}

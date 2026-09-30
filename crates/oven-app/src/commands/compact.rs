use super::{CommandContext, CommandOutcome, SlashCommand};
use crate::core::error::AppError;

pub struct Compact;

impl SlashCommand for Compact {
    fn name(&self) -> &'static str {
        "compact"
    }
    fn description(&self) -> &'static str {
        "Compact conversation history into a summary."
    }
    fn execute(
        &self,
        cx: &mut CommandContext<'_>,
        _args: &str,
    ) -> Result<CommandOutcome, AppError> {
        // Compaction rewrites the history a running turn is appending to, so
        // it asks for the driver: without one the runtime defers it.
        cx.agent()?;
        Ok(CommandOutcome::Compact)
    }
}

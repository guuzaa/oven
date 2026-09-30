use super::{CommandContext, CommandOutcome, SlashCommand};
use crate::core::error::AppError;

pub struct Exit;

impl SlashCommand for Exit {
    fn name(&self) -> &'static str {
        "exit"
    }
    fn description(&self) -> &'static str {
        "End the session."
    }
    fn execute(
        &self,
        _cx: &mut CommandContext<'_>,
        _args: &str,
    ) -> Result<CommandOutcome, AppError> {
        Ok(CommandOutcome::Exit)
    }
}

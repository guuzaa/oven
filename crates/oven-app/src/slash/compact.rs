use oven_agent::Agent;

use super::{CommandOutcome, SlashCommand};
use crate::AppError;

pub struct Compact;

impl SlashCommand for Compact {
    fn name(&self) -> &'static str {
        "compact"
    }
    fn description(&self) -> &'static str {
        "Compact conversation history into a summary."
    }
    fn execute(&self, _agent: &mut Agent, _args: &str) -> Result<CommandOutcome, AppError> {
        Ok(CommandOutcome::Compact)
    }
}

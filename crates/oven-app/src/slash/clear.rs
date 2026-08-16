use oven_agent::Agent;

use super::{CommandOutcome, SlashCommand};
use crate::AppError;

pub struct Clear;

impl SlashCommand for Clear {
    fn name(&self) -> &str {
        "clear"
    }
    fn description(&self) -> &str {
        "Clear conversation history."
    }
    fn execute(&self, agent: &mut Agent, _args: &str) -> Result<CommandOutcome, AppError> {
        agent.clear_history();
        agent.set_todos(oven_agent::TodoList::default());
        Ok(CommandOutcome::Cleared)
    }
}

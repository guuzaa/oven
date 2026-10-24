use oven_proto::TodoList;

use super::{CommandContext, CommandOutcome, SlashCommand};
use crate::core::error::AppError;

pub struct Clear;

impl SlashCommand for Clear {
    fn name(&self) -> &'static str {
        "clear"
    }
    fn description(&self) -> &'static str {
        "Clear conversation history."
    }
    fn execute(
        &self,
        cx: &mut CommandContext<'_>,
        _args: &str,
    ) -> Result<CommandOutcome, AppError> {
        let agent = cx.agent()?;
        agent.clear_history();
        agent.set_todos(TodoList::default());
        Ok(CommandOutcome::Cleared)
    }
}

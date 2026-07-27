use super::{CommandOutcome, SlashCommand};
use crate::agent::Agent;
use crate::error::AgentError;

pub struct Clear;

impl SlashCommand for Clear {
    fn name(&self) -> &str {
        "clear"
    }
    fn description(&self) -> &str {
        "Clear conversation history."
    }
    fn execute(&self, agent: &mut Agent, _args: &str) -> Result<CommandOutcome, AgentError> {
        agent.clear_history();
        Ok(CommandOutcome::Reply("history cleared".to_string()))
    }
}

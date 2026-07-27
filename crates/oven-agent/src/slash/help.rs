use super::{CommandOutcome, SlashCommand};
use crate::agent::Agent;
use crate::error::AgentError;

pub struct Help;

impl SlashCommand for Help {
    fn name(&self) -> &str {
        "help"
    }
    fn description(&self) -> &str {
        "List available slash commands."
    }
    fn execute(&self, _agent: &mut Agent, _args: &str) -> Result<CommandOutcome, AgentError> {
        let lines = [
            "Available commands:",
            "  /help        List available slash commands.",
            "  /clear       Clear conversation history.",
            "  /exit        End the session.",
        ]
        .join("\n");
        Ok(CommandOutcome::Reply(lines))
    }
}

use super::{CommandOutcome, SlashCommand};
use crate::agent::Agent;
use crate::error::AgentError;

pub struct Exit;

impl SlashCommand for Exit {
    fn name(&self) -> &str {
        "exit"
    }
    fn description(&self) -> &str {
        "End the session."
    }
    fn execute(&self, _agent: &mut Agent, _args: &str) -> Result<CommandOutcome, AgentError> {
        Ok(CommandOutcome::Exit)
    }
}

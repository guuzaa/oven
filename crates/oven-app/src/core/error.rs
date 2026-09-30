use thiserror::Error;

use crate::core::config::ConfigError;
use crate::core::session::SessionError;
use oven_agent::AgentError;
use oven_llm::ProviderError;

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("app channel closed")]
    ChannelClosed,
    /// A command asked for the conversation driver while a turn holds it.
    /// The runtime reads this as "defer the command", not as a failure.
    #[error("the agent is busy with a running turn")]
    AgentBusy,
    #[error("{0}")]
    Runtime(String),
    #[error("provider: {0}")]
    Provider(String),
    #[error("mcp: {0}")]
    Mcp(String),
}

impl From<ProviderError> for AppError {
    fn from(err: ProviderError) -> Self {
        match &err {
            ProviderError::InvalidRequest(reason) => {
                Self::Provider(format!("invalid request: {reason}"))
            }
            _ => Self::Provider(err.to_string()),
        }
    }
}

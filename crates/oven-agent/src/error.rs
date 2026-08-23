use std::fmt;

pub use oven_llm::ProviderError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentError {
    pub message: String,
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "agent error: {}", self.message)
    }
}

impl std::error::Error for AgentError {}

impl AgentError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn cancelled() -> Self {
        Self {
            message: "cancelled".to_string(),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.message == "cancelled"
    }
}

impl From<String> for AgentError {
    fn from(message: String) -> Self {
        Self { message }
    }
}

impl From<&str> for AgentError {
    fn from(message: &str) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}

impl From<ProviderError> for AgentError {
    fn from(err: ProviderError) -> Self {
        let message = match &err {
            ProviderError::UnknownModel(model) => {
                format!("model '{model}' is not available; run /setup to configure that provider")
            }
            ProviderError::NoProviderRegistered => "no provider registered; run /setup".to_string(),
            _ => format!("provider: {err}"),
        };
        Self { message }
    }
}

impl From<oven_llm::StreamCollectorError> for AgentError {
    fn from(err: oven_llm::StreamCollectorError) -> Self {
        Self {
            message: format!("stream: {}", err),
        }
    }
}

impl From<serde_json::Error> for AgentError {
    fn from(err: serde_json::Error) -> Self {
        Self {
            message: format!("json: {}", err),
        }
    }
}

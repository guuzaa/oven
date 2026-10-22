//! The failure a turn reports.

use std::fmt;

use oven_llm::ProviderError;
use serde::{Deserialize, Serialize};

pub const MAX_ITERS_EXCEEDED: &str = "agent loop exceeded max iterations";
const CANCELLED: &str = "cancelled";

/// A turn failed, and the frontend is told why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
            message: CANCELLED.to_string(),
        }
    }

    pub fn max_iters_exceeded() -> Self {
        Self {
            message: MAX_ITERS_EXCEEDED.to_string(),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.message == CANCELLED
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
            ProviderError::InvalidRequest(reason) => format!("provider: invalid request: {reason}"),
            _ => format!("provider: {err}"),
        };
        Self { message }
    }
}

impl From<oven_llm::StreamCollectorError> for AgentError {
    fn from(err: oven_llm::StreamCollectorError) -> Self {
        Self {
            message: format!("stream: {err}"),
        }
    }
}

impl From<serde_json::Error> for AgentError {
    fn from(err: serde_json::Error) -> Self {
        Self {
            message: format!("json: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_error_says_so() {
        assert!(AgentError::cancelled().is_cancelled());
        assert!(!AgentError::new("boom").is_cancelled());
    }

    #[test]
    fn the_error_survives_the_wire() {
        let error = AgentError::new("boom");
        let json = serde_json::to_string(&error).unwrap();
        assert_eq!(json, r#"{"message":"boom"}"#);
        assert_eq!(serde_json::from_str::<AgentError>(&json).unwrap(), error);
    }
}

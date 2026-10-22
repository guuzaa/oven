//! The persisted shape of a conversation.
//!
//! One [`Record`] per JSONL line, so a session file appends without rewriting
//! what came before. The format is part of the protocol: a frontend that
//! reloads a session reads the same records the runtime wrote.

use oven_llm::{Message, Usage};
use serde::{Deserialize, Serialize};

use crate::TodoItem;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    Message {
        /// Unix milliseconds when the message was created.
        timestamp: u64,
        #[serde(flatten)]
        message: Message,
    },
    TokenUsage {
        /// Unix milliseconds; the timestamp of the assistant message this
        /// usage belongs to.
        timestamp: u64,
        #[serde(flatten)]
        usage: Usage,
    },
    /// Thinking span for the preceding assistant message. `timestamp` is when
    /// thinking started; `duration_ms` is wall time until thinking ended.
    Thinking { timestamp: u64, duration_ms: u64 },
    /// Session-level metadata, written as the first line of a session file.
    SessionMeta(SessionMeta),
    TodoList {
        timestamp: u64,
        items: Vec<TodoItem>,
    },
}

/// Where and when a session was created. Written as the first JSONL record so
/// the workspace root survives a resume.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMeta {
    pub root: String,
    /// Unix milliseconds when the session first got content.
    pub created_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_survives_the_wire() {
        let record = Record::Thinking {
            timestamp: 1,
            duration_ms: 2,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(json, r#"{"type":"thinking","timestamp":1,"duration_ms":2}"#);
    }
}

//! The persisted shape of a conversation.
//!
//! One [`Record`] per JSONL line, so a session file appends without rewriting
//! what came before. Here are the rules for projecting a [`History`] into
//! records: which message closes a turn, where that turn's usage lands and
//! when a thinking span is worth a line of its own.

use oven_llm::{Message, Role, Usage};
use serde::{Deserialize, Serialize};

use super::History;
use crate::core::todo::TodoItem;

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

impl History {
    /// The conversation as persistence-ready records: every message plus a
    /// `TokenUsage` record right after the final assistant message of each
    /// turn that produced a response. Zero-usage turns emit no record, and a
    /// leading system message is kept without usage. Timestamps are the
    /// original ones, so a rewind that rewrites the file doesn't restamp
    /// older messages.
    pub fn records(&self) -> Vec<Record> {
        self.records_from(0)
    }

    /// [`records`](Self::records) covering only the messages from index
    /// `from` onward, so a caller that already persisted the earlier ones
    /// (the app layer appending after each turn) pays only for what is new.
    /// A turn whose final assistant message falls past `from` still emits
    /// its usage record, so the file stays identical to a full rewrite.
    pub fn records_from(&self, from: usize) -> Vec<Record> {
        let from = from.min(self.messages.len());
        let mut out = Vec::with_capacity(
            self.messages.len() - from + self.turn_usage.len() + self.thinking.len() + 1,
        );
        if from == 0
            && let Some(meta) = &self.meta
        {
            out.push(Record::SessionMeta(meta.clone()));
        }
        let turn_final = self.turn_final_assistants();
        for (i, (message, timestamp)) in self.messages.iter().enumerate().skip(from) {
            out.push(Record::Message {
                timestamp: *timestamp,
                message: (**message).clone(),
            });
            self.push_thinking_record(&mut out, i);
            if let Some(turn) = turn_final[i]
                && let Some((usage, timestamp)) = self.turn_usage.get(turn)
                && *usage != Usage::default()
            {
                out.push(Record::TokenUsage {
                    timestamp: *timestamp,
                    usage: *usage,
                });
            }
        }
        out
    }

    /// The turn each message's usage record belongs to: `Some(turn)` on the
    /// last assistant message of turn `turn`, `None` on every other message.
    /// `Role::Tool` messages belong to the turn of the tool call they answer,
    /// so only a new `Role::User` message closes a turn.
    fn turn_final_assistants(&self) -> Vec<Option<usize>> {
        let mut final_of = vec![None; self.messages.len()];
        let mut turn = None;
        let mut last_assistant = None;
        for (i, (message, _)) in self.messages.iter().enumerate() {
            match message.role {
                Role::User => {
                    if let (Some(assistant), Some(turn)) = (last_assistant.take(), turn) {
                        final_of[assistant] = Some(turn);
                    }
                    turn = Some(turn.map_or(0, |turn| turn + 1));
                }
                Role::Assistant => last_assistant = Some(i),
                _ => {}
            }
        }
        if let (Some(assistant), Some(turn)) = (last_assistant, turn) {
            final_of[assistant] = Some(turn);
        }
        final_of
    }

    fn push_thinking_record(&self, out: &mut Vec<Record>, i: usize) {
        if let Some((duration_ms, timestamp)) = self.thinking.get(i).copied().flatten() {
            out.push(Record::Thinking {
                timestamp,
                duration_ms,
            });
        }
    }
}

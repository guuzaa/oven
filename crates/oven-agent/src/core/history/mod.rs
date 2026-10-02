//! The conversation buffer, and the records it is persisted as.
//!
//! This module owns the message list itself — pushing, rewinding, the last
//! turn's usage, the thinking spans. [`record`](self::record) owns the
//! projection a session file stores.

pub mod record;

use std::sync::Arc;

use oven_host::now_ms;
use oven_llm::{Message, Role, Usage};

pub use record::{Record, SessionMeta};

type Timestamp = u64;

/// Conversation history with API-reported token tracking.
///
/// Usage accounting stores the final provider response of each user turn
/// (`turn_usage`). A single `TokenUsage` record is persisted after the
/// turn's final assistant message.
///
/// The `revision` is bumped whenever the message list is structurally replaced
/// (`clear` / `set_messages_with_records`). The App layer
/// uses this to detect an in-memory reset so it can keep the persisted
/// session store untouched and resume appending after it.
#[derive(Debug)]
pub struct History {
    messages: Vec<(Arc<Message>, Timestamp)>,
    turn_usage: Vec<(Usage, Timestamp)>,
    /// Per-message thinking span `(duration_ms, started_at)`, aligned with
    /// `messages`. `None` when that message had no timed thinking.
    thinking: Vec<Option<(u64, Timestamp)>>,
    revision: u64,
    meta: Option<SessionMeta>,
}

impl History {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            turn_usage: Vec::new(),
            thinking: Vec::new(),
            revision: 0,
            meta: None,
        }
    }

    pub fn push(&mut self, m: Message) {
        if m.role == Role::User {
            self.turn_usage.push((Usage::default(), 0));
        }
        self.messages.push((Arc::new(m), now_ms()));
        self.thinking.push(None);
    }

    pub fn insert_system(&mut self, m: Message) {
        self.messages.insert(0, (Arc::new(m), now_ms()));
        self.thinking.insert(0, None);
    }

    pub fn clear(&mut self) {
        self.revision += 1;
        self.messages.clear();
        self.turn_usage.clear();
        self.thinking.clear();
        self.meta = None;
    }

    /// Record the session's workspace root if it is not already known (a
    /// resumed session keeps its original root and creation time).
    pub fn ensure_session_meta(&mut self, root: String) {
        if self.meta.is_none() {
            self.meta = Some(SessionMeta {
                root,
                created_at: now_ms(),
            });
        }
    }

    pub fn session_meta(&self) -> Option<&SessionMeta> {
        self.meta.as_ref()
    }

    /// Replace the entire history from a persisted session: messages and the
    /// `TokenUsage` records that follow each turn's final assistant message.
    /// If a turn carries several usage records (legacy files), the last one is
    /// used.
    pub fn set_messages_with_records(&mut self, records: Vec<Record>) {
        self.revision += 1;
        self.messages.clear();
        self.turn_usage.clear();
        self.thinking.clear();
        self.meta = None;
        for record in records {
            match record {
                Record::Message { timestamp, message } => {
                    if message.role == Role::User {
                        self.turn_usage.push((Usage::default(), 0));
                    }
                    self.messages.push((Arc::new(message), timestamp));
                    self.thinking.push(None);
                }
                Record::TokenUsage { timestamp, usage } => match self.turn_usage.last_mut() {
                    Some(last) => {
                        last.0 = usage;
                        last.1 = timestamp;
                    }
                    None => self.turn_usage.push((usage, timestamp)),
                },
                Record::Thinking {
                    timestamp,
                    duration_ms,
                } => {
                    if let Some(slot) = self.thinking.last_mut() {
                        *slot = Some((duration_ms, timestamp));
                    }
                }
                Record::SessionMeta(meta) => self.meta = Some(meta),
                Record::TodoList { .. } => {}
            }
        }
    }

    /// Remove the last user turn (the user message and everything after it),
    /// returning the removed user message. Returns `None` when there is no
    /// user message to rewind.
    pub fn rewind_last_turn(&mut self) -> Option<Message> {
        let idx = self
            .messages
            .iter()
            .rposition(|(m, _)| m.role == Role::User)?;
        let removed = self.messages.drain(idx..).next()?;
        let _ = self.thinking.drain(idx..);
        let _ = self.turn_usage.pop();
        Some(Arc::try_unwrap(removed.0).unwrap_or_else(|arc| (*arc).clone()))
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The conversation messages in order.
    pub fn messages(&self) -> impl ExactSizeIterator<Item = &Message> + '_ {
        self.messages.iter().map(|(m, _)| &**m)
    }

    /// The text of the first system message in the conversation. A resumed
    /// session drops persisted system messages, so one is only here when a
    /// caller pushed it explicitly.
    pub fn system_message(&self) -> Option<String> {
        self.messages()
            .find(|m| m.role == Role::System)
            .and_then(|m| m.system_prompt())
    }

    /// The messages as shared handles. A snapshot of the conversation then
    /// costs one refcount per message instead of copying every message.
    pub fn shared_messages(&self) -> impl ExactSizeIterator<Item = &Arc<Message>> + '_ {
        self.messages.iter().map(|(m, _)| m)
    }

    /// Messages paired with the `Record` timestamps they were stored with
    /// and any thinking duration recorded for that message.
    pub fn iter_timed(&self) -> impl ExactSizeIterator<Item = (&Message, u64, Option<u64>)> + '_ {
        self.messages
            .iter()
            .zip(self.thinking.iter())
            .map(|((m, ts), th)| (&**m, *ts, th.map(|(d, _)| d)))
    }

    /// Attach a thinking span to the last message. No-op when `duration_ms`
    /// is zero or history is empty.
    pub fn record_thinking(&mut self, started_at: u64, duration_ms: u64) {
        if duration_ms == 0 {
            return;
        }
        if let Some(slot) = self.thinking.last_mut() {
            *slot = Some((duration_ms, started_at));
        }
    }

    /// Timestamp of the current turn's user message, if any.
    pub fn last_user_timestamp(&self) -> Option<u64> {
        self.messages
            .iter()
            .rev()
            .find(|(m, _)| m.role == Role::User)
            .map(|(_, ts)| *ts)
    }

    /// Wall-clock elapsed milliseconds of the current turn: `now` minus the
    /// user message timestamp persisted on that `Record`.
    pub fn elapsed_ms(&self) -> u64 {
        self.last_user_timestamp()
            .map_or(0, |start| now_ms().saturating_sub(start))
    }

    /// Persisted duration of the last user turn: last message timestamp minus
    /// the user message timestamp. `None` when there is no user turn.
    pub fn last_turn_duration_ms(&self) -> Option<u64> {
        let idx = self
            .messages
            .iter()
            .rposition(|(m, _)| m.role == Role::User)?;
        let start = self.messages[idx].1;
        let end = self.messages[idx..].last().map_or(start, |(_, ts)| *ts);
        Some(end.saturating_sub(start))
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Message> + '_ {
        self.messages()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Usage of the current (last) user turn, or zero when there is none.
    /// Matches the `TokenUsage` record persisted for that turn.
    pub fn last_turn_usage(&self) -> Usage {
        self.turn_usage.last().map(|(u, _)| *u).unwrap_or_default()
    }

    /// Record a provider response's usage as the current turn's latest usage.
    pub fn record_usage(&mut self, usage: &Usage) {
        let timestamp = self.messages.last().map_or(0, |(_, ts)| *ts);
        match self.turn_usage.last_mut() {
            Some(last) => {
                last.0 = *usage;
                last.1 = timestamp;
            }
            None => self.turn_usage.push((*usage, timestamp)),
        }
        tracing::debug!(
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            cache_read_tokens = usage.cache_read_tokens,
            "turn usage recorded"
        );
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Index<usize> for History {
    type Output = Message;
    fn index(&self, i: usize) -> &Message {
        &self.messages[i].0
    }
}

#[cfg(test)]
#[path = "history_test.rs"]
mod tests;

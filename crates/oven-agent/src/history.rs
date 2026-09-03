use oven_host::now_ms;
use oven_llm::{Message, Role, Usage};
use serde::{Deserialize, Serialize};

use crate::todo::TodoItem;

type Timestamp = u64;

/// One persisted conversation record: a message or the token usage of a turn.
///
/// Sessions are stored as one JSON record per line. Messages no longer carry
/// a per-message usage slot; instead a single `TokenUsage` record is written
/// right after the final assistant message of each user turn, holding the
/// sum of that turn's provider responses. A leading `SessionMeta` record
/// records the workspace root the session was created in.
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

/// Conversation history with API-reported token tracking.
///
/// Usage accounting accumulates every provider response of a user turn
/// (`turn_usage`). A single `TokenUsage` record is persisted after the
/// turn's final assistant message, holding that sum, so the in-memory total
/// always equals the sum of the persisted usage records.
///
/// The `revision` is bumped whenever the message list is structurally replaced
/// (`clear` / `set_messages_with_records`). The App layer
/// uses this to detect an in-memory reset so it can keep the persisted
/// session store untouched and resume appending after it.
#[derive(Debug)]
pub struct History {
    messages: Vec<(Message, Timestamp)>,
    turn_usage: Vec<(Usage, Timestamp)>,
    revision: u64,
    meta: Option<SessionMeta>,
}

impl History {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            turn_usage: Vec::new(),
            revision: 0,
            meta: None,
        }
    }

    pub fn push(&mut self, m: Message) {
        if m.role == Role::User {
            self.turn_usage.push((Usage::default(), 0));
        }
        self.messages.push((m, now_ms()));
    }

    pub fn insert_system(&mut self, m: Message) {
        self.messages.insert(0, (m, now_ms()));
    }

    pub fn clear(&mut self) {
        self.revision += 1;
        self.messages.clear();
        self.turn_usage.clear();
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
    /// The cumulative total is recomputed as the sum of the per-turn usage,
    /// so a resumed session keeps its counters. If a turn carries several
    /// usage records (legacy files), they are summed.
    pub fn set_messages_with_records(&mut self, records: Vec<Record>) {
        self.revision += 1;
        self.messages.clear();
        self.turn_usage.clear();
        self.meta = None;
        for record in records {
            match record {
                Record::Message { timestamp, message } => {
                    if message.role == Role::User {
                        self.turn_usage.push((Usage::default(), 0));
                    }
                    self.messages.push((message, timestamp));
                }
                Record::TokenUsage { timestamp, usage } => match self.turn_usage.last_mut() {
                    Some(last) => {
                        last.0 += usage;
                        last.1 = timestamp;
                    }
                    None => self.turn_usage.push((usage, timestamp)),
                },
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
        let removed = self.messages.drain(idx..).next().map(|(m, _)| m)?;
        let _ = self.turn_usage.pop();
        Some(removed)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The conversation messages in order.
    pub fn messages(&self) -> impl ExactSizeIterator<Item = &Message> + '_ {
        self.messages.iter().map(|(m, _)| m)
    }

    /// The conversation as persistence-ready records: every message plus a
    /// `TokenUsage` record right after the final assistant message of each
    /// turn that produced a response. Zero-usage turns emit no record, and a
    /// leading system message is kept without usage. Timestamps are the
    /// original ones, so a rewind that rewrites the file doesn't restamp
    /// older messages.
    pub fn records(&self) -> Vec<Record> {
        let mut out = Vec::with_capacity(self.messages.len() + self.turn_usage.len() + 1);
        if let Some(meta) = &self.meta {
            out.push(Record::SessionMeta(meta.clone()));
        }
        let first_user = self
            .messages
            .iter()
            .position(|(m, _)| m.role == Role::User)
            .unwrap_or(self.messages.len());
        for (message, timestamp) in &self.messages[..first_user] {
            out.push(Record::Message {
                timestamp: *timestamp,
                message: message.clone(),
            });
        }
        let user_starts: Vec<usize> = self
            .messages
            .iter()
            .enumerate()
            .filter(|(_, (m, _))| m.role == Role::User)
            .map(|(i, _)| i)
            .collect();
        for (k, &start) in user_starts.iter().enumerate() {
            let end = user_starts
                .get(k + 1)
                .copied()
                .unwrap_or(self.messages.len());
            let last_assistant = self.messages[start..end]
                .iter()
                .rposition(|(m, _)| m.role == Role::Assistant)
                .map(|j| start + j);
            for (i, (message, timestamp)) in self.messages[start..end].iter().enumerate() {
                let i = start + i;
                out.push(Record::Message {
                    timestamp: *timestamp,
                    message: message.clone(),
                });
                if last_assistant == Some(i)
                    && let Some((usage, timestamp)) = self.turn_usage.get(k)
                    && *usage != Usage::default()
                {
                    out.push(Record::TokenUsage {
                        timestamp: *timestamp,
                        usage: *usage,
                    });
                }
            }
        }
        out
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

    /// Record a provider response's usage. Accumulates the response into the
    /// current turn.
    pub fn record_usage(&mut self, usage: &Usage) {
        if self.turn_usage.is_empty() {
            self.turn_usage.push((Usage::default(), 0));
        }
        let last = self.turn_usage.last_mut().expect("usage bucket exists");
        last.0 += *usage;
        last.1 = self.messages.last().map(|(_, ts)| *ts).unwrap_or(0);
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
mod tests {
    use oven_llm::ContentBlock;

    use super::*;

    fn usage(input: u32) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: 10,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
        }
    }

    fn assistant_tools(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message::assistant(vec![ContentBlock::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
        }])
    }

    fn usage_inputs(h: &History) -> Vec<u32> {
        h.records()
            .iter()
            .filter_map(|r| match r {
                Record::TokenUsage { usage, .. } => Some(usage.input_tokens),
                _ => None,
            })
            .collect()
    }

    fn record_kinds(records: &[Record]) -> Vec<&str> {
        records
            .iter()
            .map(|r| match r {
                Record::Message { .. } => "msg",
                Record::TokenUsage { .. } => "usage",
                Record::SessionMeta(_) => "meta",
                Record::TodoList { .. } => "todo_list",
            })
            .collect()
    }

    fn assert_records_equal(a: &[Record], b: &[Record]) {
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b) {
            match (x, y) {
                (
                    Record::Message {
                        timestamp: t1,
                        message: m1,
                    },
                    Record::Message {
                        timestamp: t2,
                        message: m2,
                    },
                ) => {
                    assert_eq!(t1, t2);
                    assert_eq!(m1.role, m2.role);
                    assert_eq!(m1.content.len(), m2.content.len());
                }
                (
                    Record::TokenUsage {
                        timestamp: t1,
                        usage: u1,
                    },
                    Record::TokenUsage {
                        timestamp: t2,
                        usage: u2,
                    },
                ) => {
                    assert_eq!(t1, t2);
                    assert_eq!(u1, u2);
                }
                (Record::SessionMeta(m1), Record::SessionMeta(m2)) => {
                    assert_eq!(m1, m2);
                }
                (
                    Record::TodoList {
                        timestamp: t1,
                        items: i1,
                    },
                    Record::TodoList {
                        timestamp: t2,
                        items: i2,
                    },
                ) => {
                    assert_eq!(t1, t2);
                    assert_eq!(i1, i2);
                }
                _ => panic!("record kind mismatch"),
            }
        }
    }

    #[test]
    fn last_turn_usage_is_the_current_bucket() {
        let mut h = History::new();
        assert_eq!(h.last_turn_usage(), Usage::default());

        h.push(Message::user_text("first"));
        h.push(Message::assistant_text("one"));
        h.record_usage(&usage(100));
        assert_eq!(h.last_turn_usage().input_tokens, 100);

        h.push(Message::user_text("second"));
        h.push(Message::assistant_text("two"));
        h.record_usage(&usage(200));
        assert_eq!(h.last_turn_usage().input_tokens, 200);

        h.rewind_last_turn();
        assert_eq!(h.last_turn_usage().input_tokens, 100);
    }

    #[test]
    fn record_usage_accumulates_every_response() {
        let mut h = History::new();
        h.push(Message::user_text("hi"));
        h.record_usage(&usage(100));
        assert_eq!(h.last_turn_usage().input_tokens, 100);

        h.record_usage(&usage(150));
        assert_eq!(h.last_turn_usage().input_tokens, 250);
    }

    #[test]
    fn clear_resets_total_usage() {
        let mut h = History::new();
        h.push(Message::user_text("hi"));
        h.record_usage(&usage(100));

        h.clear();

        assert!(h.is_empty());
        assert_eq!(h.last_turn_usage().input_tokens, 0);
        assert_eq!(h.last_turn_usage().output_tokens, 0);
    }

    #[test]
    fn rewind_removes_last_turn_and_returns_user_message() {
        let mut h = History::new();
        h.push(Message::user_text("first"));
        h.push(Message::assistant(vec![ContentBlock::text("one")]));
        h.push(Message::user_text("second"));
        h.push(assistant_tools(
            "c1",
            "bash",
            serde_json::json!({ "command": "ls" }),
        ));
        h.push(Message::tool_result("c1", "out", false));
        h.record_usage(&usage(100));

        let removed = h.rewind_last_turn().unwrap();
        assert_eq!(removed.role, Role::User);
        assert!(matches!(&removed.content[0], ContentBlock::Text { text } if text == "second"));

        let roles: Vec<Role> = h.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant]);
        assert!(matches!(&h[0].content[0], ContentBlock::Text { text } if text == "first"));
    }

    #[test]
    fn rewind_of_interrupted_turn_removes_only_user_message() {
        let mut h = History::new();
        h.push(Message::user_text("ping"));

        let removed = h.rewind_last_turn().unwrap();
        assert!(matches!(&removed.content[0], ContentBlock::Text { text } if text == "ping"));
        assert!(h.is_empty());
    }

    #[test]
    fn rewind_rolls_back_only_the_removed_turn_usage() {
        let mut h = History::new();
        h.push(Message::user_text("first"));
        h.push(Message::assistant_text("one"));
        h.record_usage(&usage(100));
        h.push(Message::user_text("second"));
        h.push(assistant_tools(
            "c1",
            "bash",
            serde_json::json!({ "command": "ls" }),
        ));
        h.push(Message::tool_result("c1", "out", false));
        h.record_usage(&usage(200));

        assert!(h.rewind_last_turn().is_some());
        assert!(h.rewind_last_turn().is_some());
        assert!(h.is_empty());
        assert!(h.rewind_last_turn().is_none());
    }

    #[test]
    fn rewind_rolls_back_the_turn_accumulated_usage() {
        let mut h = History::new();
        h.push(Message::user_text("first"));
        h.push(Message::assistant(vec![ContentBlock::text("one")]));
        h.record_usage(&usage(10));
        h.push(Message::user_text("second"));
        h.push(assistant_tools(
            "c1",
            "bash",
            serde_json::json!({ "command": "ls" }),
        ));
        h.push(Message::tool_result("c1", "out", false));
        h.record_usage(&usage(100));
        h.push(Message::assistant(vec![ContentBlock::text("two")]));
        h.record_usage(&usage(50));
        assert_eq!(usage_inputs(&h), vec![10, 150]);

        h.rewind_last_turn();
        assert_eq!(h.last_turn_usage().input_tokens, 10);
    }

    #[test]
    fn rewind_repeats_until_history_is_empty() {
        let mut h = History::new();
        h.push(Message::user_text("first"));
        h.push(Message::assistant(vec![ContentBlock::text("one")]));
        h.push(Message::user_text("second"));
        h.push(assistant_tools(
            "c1",
            "bash",
            serde_json::json!({ "command": "ls" }),
        ));
        h.push(Message::tool_result("c1", "out", false));

        assert!(h.rewind_last_turn().is_some());
        assert_eq!(h.len(), 2);
        assert!(h.rewind_last_turn().is_some());
        assert!(h.is_empty());
        assert!(h.rewind_last_turn().is_none());
    }

    #[test]
    fn rewind_on_empty_history_returns_none() {
        let mut h = History::new();
        assert!(h.rewind_last_turn().is_none());
        assert!(h.is_empty());
    }

    #[test]
    fn records_emit_one_token_usage_after_each_turn_final_assistant() {
        let mut h = History::new();
        h.insert_system(Message::system("s"));
        h.push(Message::user_text("first"));
        h.push(Message::assistant(vec![ContentBlock::text("one")]));
        h.record_usage(&usage(100));

        h.push(Message::user_text("second"));
        h.push(assistant_tools(
            "c1",
            "bash",
            serde_json::json!({ "command": "ls" }),
        ));
        h.push(Message::tool_result("c1", "out", false));
        h.push(Message::assistant(vec![ContentBlock::text("two")]));
        h.record_usage(&usage(50));
        h.record_usage(&usage(75));

        let records = h.records();
        assert_eq!(
            record_kinds(&records),
            vec![
                "msg", // system
                "msg", "msg", "usage", // first turn
                "msg", "msg", "msg", "msg", "usage", // second turn incl. tool chain
            ]
        );
        assert_eq!(usage_inputs(&h), vec![100, 125]);

        // Each usage record shares the timestamp of the assistant message it
        // follows.
        let (Record::Message { timestamp: t1, .. }, Record::TokenUsage { timestamp: u1, .. }) =
            (&records[2], &records[3])
        else {
            panic!("expected assistant + usage after first turn");
        };
        assert_eq!(u1, t1);
        let (Record::Message { timestamp: t2, .. }, Record::TokenUsage { timestamp: u2, .. }) =
            (&records[7], &records[8])
        else {
            panic!("expected assistant + usage after second turn");
        };
        assert_eq!(u2, t2);
    }

    #[test]
    fn records_skip_zero_usage_turns() {
        let mut h = History::new();
        h.push(Message::user_text("no response"));
        h.push(Message::user_text("answered"));
        h.push(Message::assistant(vec![ContentBlock::text("hi")]));
        h.record_usage(&usage(7));

        let records = h.records();
        assert_eq!(record_kinds(&records), vec!["msg", "msg", "msg", "usage"]);
        assert_eq!(usage_inputs(&h), vec![7]);
    }

    #[test]
    fn records_roundtrip_preserves_usage_and_timestamps() {
        let mut h = History::new();
        h.push(Message::user_text("a"));
        h.push(Message::assistant(vec![ContentBlock::text("b")]));
        h.record_usage(&usage(11));
        h.push(Message::user_text("c"));
        h.push(assistant_tools("t", "bash", serde_json::json!({})));
        h.push(Message::tool_result("t", "r", false));
        h.push(Message::assistant(vec![ContentBlock::text("done")]));
        h.record_usage(&usage(22));

        let records = h.records();
        let mut restored = History::new();
        restored.set_messages_with_records(records.clone());
        assert_records_equal(&restored.records(), &records);
        assert_eq!(restored.last_turn_usage().input_tokens, 22);

        // Rewind works on the restored history and rolls back one turn at a
        // time using the persisted usage.
        assert!(restored.rewind_last_turn().is_some());
        assert_eq!(restored.last_turn_usage().input_tokens, 11);
        assert!(restored.rewind_last_turn().is_some());
        assert_eq!(restored.last_turn_usage().input_tokens, 0);
    }

    #[test]
    fn session_meta_roundtrips_and_survives_clear() {
        let mut h = History::new();
        assert!(h.session_meta().is_none());

        h.ensure_session_meta("/ws".into());
        let first = h.session_meta().unwrap().clone();
        assert_eq!(first.root, "/ws");
        assert!(first.created_at > 0);

        // ensure is a no-op once meta is known.
        h.ensure_session_meta("/other".into());
        assert_eq!(h.session_meta().unwrap().root, "/ws");

        h.push(Message::user_text("a"));
        let records = h.records();
        assert_eq!(record_kinds(&records).first(), Some(&"meta"));
        let mut restored = History::new();
        restored.set_messages_with_records(records.clone());
        assert_eq!(restored.session_meta(), Some(&first));
        assert_records_equal(&restored.records(), &records);

        // /clear drops the meta so a fresh session records its own root.
        restored.clear();
        assert!(restored.session_meta().is_none());
        restored.ensure_session_meta("/other".into());
        assert_eq!(restored.session_meta().unwrap().root, "/other");
    }

    #[test]
    fn restore_sums_usage_records_of_a_turn() {
        let records = vec![
            Record::Message {
                timestamp: 1,
                message: Message::user_text("a"),
            },
            Record::Message {
                timestamp: 2,
                message: Message::assistant(vec![ContentBlock::text("b")]),
            },
            Record::TokenUsage {
                timestamp: 3,
                usage: usage(100),
            },
            Record::Message {
                timestamp: 4,
                message: Message::assistant(vec![ContentBlock::text("c")]),
            },
            Record::TokenUsage {
                timestamp: 5,
                usage: usage(50),
            },
        ];
        let mut h = History::new();
        h.set_messages_with_records(records);
        assert_eq!(usage_inputs(&h), vec![150]);
    }

    #[test]
    fn push_stamps_messages_with_timestamps() {
        let mut h = History::new();
        h.push(Message::user_text("hi"));
        let records = h.records();
        let Record::Message { timestamp, .. } = &records[0] else {
            panic!("expected message record");
        };
        assert!(*timestamp > 0);
    }

    #[test]
    fn records_never_emit_todo_list() {
        use crate::todo::{TodoItem, TodoStatus};

        let records = vec![
            Record::Message {
                timestamp: 1,
                message: Message::user_text("hi"),
            },
            Record::TodoList {
                timestamp: 2,
                items: vec![TodoItem {
                    id: "a".into(),
                    content: "one".into(),
                    status: TodoStatus::Pending,
                }],
            },
            Record::Message {
                timestamp: 3,
                message: Message::assistant(vec![ContentBlock::text("ok")]),
            },
        ];
        let mut h = History::new();
        h.set_messages_with_records(records);
        assert_eq!(h.len(), 2);
        assert_eq!(record_kinds(&h.records()), vec!["msg", "msg"]);
        assert!(
            !h.records()
                .iter()
                .any(|r| matches!(r, Record::TodoList { .. }))
        );
    }
}

use oven_llm::{ContentBlock, Message, Role, Usage};

/// Conversation history with API-reported token tracking.
///
/// The conversation size (`last_prompt_tokens`) comes from the provider's
/// last response, not a local estimate. Between calls we don't try to
/// measure appended user/tool messages; we assume a single user message
/// adds a negligible number of tokens relative to the budget, and refresh
/// the count from the next API response.
///
/// The `revision` is bumped whenever the message list is structurally replaced
/// (`clear` / `set_messages`). The App layer uses this to detect an in-memory
/// reset so it can keep the persisted session store untouched and resume
/// appending after it.
#[derive(Debug)]
pub struct History {
    messages: Vec<Message>,
    last_prompt_tokens: usize,
    /// Per-turn cumulative usage, one entry per `Role::User` message in
    /// `messages`. Kept in sync with every mutation so `rewind_last_turn`
    /// can subtract exactly the usage the rolled-back turn consumed.
    turn_usage: Vec<Usage>,
    total: Usage,
    revision: u64,
}

impl History {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            last_prompt_tokens: 0,
            turn_usage: Vec::new(),
            total: Usage::default(),
            revision: 0,
        }
    }

    pub fn push(&mut self, m: Message) {
        let starts_turn = m.role == Role::User;
        self.messages.push(m);
        if starts_turn {
            self.turn_usage.push(Usage::default());
        }
    }

    pub fn insert_system(&mut self, m: Message) {
        self.messages.insert(0, m);
    }

    pub fn clear(&mut self) {
        self.revision += 1;
        self.messages.clear();
        self.last_prompt_tokens = 0;
        self.turn_usage.clear();
        self.total = Usage::default();
    }

    pub fn set_messages(&mut self, msgs: Vec<Message>) {
        self.revision += 1;
        let turns = msgs.iter().filter(|m| m.role == Role::User).count();
        self.messages = msgs;
        self.last_prompt_tokens = 0;
        self.turn_usage = vec![Usage::default(); turns];
        self.total = Usage::default();
    }

    /// Remove the last user turn (the user message and everything after it),
    /// returning the removed user message. Returns `None` when there is no
    /// user message to rewind. The cached prompt-token count is cleared so
    /// the next provider call refreshes the conversation-size estimate, and
    /// the removed turn's cumulative usage is rolled back out of `total`.
    pub fn rewind_last_turn(&mut self) -> Option<Message> {
        let idx = self.messages.iter().rposition(|m| m.role == Role::User)?;
        let removed = self.messages.drain(idx..).next()?;
        if let Some(turn) = self.turn_usage.pop() {
            self.total -= turn;
        }
        self.last_prompt_tokens = 0;
        Some(removed)
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Message> {
        self.messages.iter()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Last reported prompt-token count from the provider (0 before the
    /// first call or after a trim).
    pub fn prompt_tokens(&self) -> usize {
        self.last_prompt_tokens
    }

    /// Cumulative usage across all recorded responses.
    pub fn total_usage(&self) -> &Usage {
        &self.total
    }

    /// Record a provider response's usage. Refreshes `last_prompt_tokens`
    /// (the current conversation size), accumulates cumulative totals, and
    /// attributes the usage to the turn currently being run so a later
    /// rewind can roll it back.
    pub fn record_usage(&mut self, usage: &Usage) {
        self.last_prompt_tokens = usage.input_tokens as usize;
        self.total += *usage;
        if let Some(turn) = self.turn_usage.last_mut() {
            *turn += *usage;
        }
    }

    /// Drop the oldest non-system turn when the last API-reported
    /// prompt-token count exceeds `budget`. We rely on the reported count
    /// rather than a local estimate; after draining we clear the cached
    /// count and let the next provider call refresh it.
    pub fn trim_to_budget(&mut self, budget: usize) {
        if self.messages.is_empty() || self.last_prompt_tokens <= budget {
            return;
        }
        let starts = turn_starts(&self.messages);
        let first_starts_system = self
            .messages
            .first()
            .is_some_and(|m| m.role == Role::System);
        let first_removable = if first_starts_system { 1 } else { 0 };
        if first_removable >= starts.len() {
            return;
        }
        let start = starts[first_removable];
        let end = if first_removable + 1 < starts.len() {
            starts[first_removable + 1]
        } else {
            self.messages.len()
        };
        let removed_turns = self
            .messages
            .drain(start..end)
            .filter(|m| m.role == Role::User)
            .count();
        if removed_turns > 0 {
            self.turn_usage.drain(..removed_turns);
        }
        self.last_prompt_tokens = 0;
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
        &self.messages[i]
    }
}

fn has_tool_use(m: &Message) -> bool {
    m.content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
}

/// A "turn" is a starter message (system, user, or assistant carrying tool
/// calls) plus any following tool-result messages until the next starter.
/// Returns the indices that begin each turn, including the leading system
/// block as a single turn.
fn turn_starts(history: &[Message]) -> Vec<usize> {
    let mut starts = Vec::new();
    for (i, m) in history.iter().enumerate() {
        let is_starter = match m.role {
            Role::System | Role::User => true,
            Role::Assistant => has_tool_use(m) || i == 0,
            Role::Tool => false,
        };
        if is_starter {
            starts.push(i);
        }
    }
    starts
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn record_usage_refreshes_and_accumulates() {
        let mut h = History::new();
        h.push(Message::user_text("hi"));
        h.record_usage(&usage(100));
        assert_eq!(h.prompt_tokens(), 100);
        assert_eq!(h.total_usage().input_tokens, 100);
        assert_eq!(h.total_usage().output_tokens, 10);

        h.record_usage(&usage(150));
        assert_eq!(h.prompt_tokens(), 150);
        assert_eq!(h.total_usage().input_tokens, 250);
        assert_eq!(h.total_usage().output_tokens, 20);
    }

    #[test]
    fn clear_resets_total_usage() {
        let mut h = History::new();
        h.push(Message::user_text("hi"));
        h.record_usage(&usage(100));

        h.clear();

        assert!(h.is_empty());
        assert_eq!(h.total_usage().input_tokens, 0);
        assert_eq!(h.total_usage().output_tokens, 0);
    }

    #[test]
    fn trim_drops_oldest_user_turn_when_over_budget() {
        let mut h = History::new();
        h.insert_system(Message::system("system"));
        for _ in 0..5 {
            h.push(Message::user_text("x".repeat(1600)));
        }
        h.record_usage(&usage(1000));
        h.trim_to_budget(700);
        assert_eq!(h.messages()[0].role, Role::System);
        let remaining_user = h.iter().filter(|m| m.role == Role::User).count();
        assert!(remaining_user < 5);
        assert_eq!(h.prompt_tokens(), 0);
    }

    #[test]
    fn trim_preserves_system_turn() {
        let mut h = History::new();
        h.insert_system(Message::system("system"));
        h.push(Message::user_text("hi"));
        h.record_usage(&usage(100));
        h.trim_to_budget(10);
        assert_eq!(h.messages()[0].role, Role::System);
        assert!(h.iter().filter(|m| m.role == Role::User).count() <= 1);
    }

    #[test]
    fn trim_no_op_when_under_budget() {
        let mut h = History::new();
        h.push(Message::user_text("hi"));
        h.record_usage(&usage(50));
        let before = h.len();
        h.trim_to_budget(100);
        assert_eq!(h.len(), before);
        assert_eq!(h.prompt_tokens(), 50);
    }

    #[test]
    fn trim_keeps_tool_results_with_their_assistant_turn() {
        let mut h = History::new();
        h.insert_system(Message::system("s"));
        h.push(assistant_tools(
            "c1",
            "file_read",
            serde_json::json!({"path": "a"}),
        ));
        h.push(Message::tool_result("c1", "x".repeat(1600), false));
        h.push(assistant_tools(
            "c1",
            "file_read",
            serde_json::json!({"path": "a"}),
        ));
        h.push(Message::tool_result("c1", "small result", false));
        h.record_usage(&usage(1000));
        h.trim_to_budget(200);
        for (i, m) in h.iter().enumerate() {
            if m.role == Role::Tool {
                assert!(
                    i > 0 && h[i - 1].role == Role::Assistant && has_tool_use(&h[i - 1]),
                    "orphan tool result at index {i}"
                );
            }
        }
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
        assert_eq!(h.prompt_tokens(), 0);
        assert_eq!(h.total_usage().input_tokens, 0);
        assert_eq!(h.total_usage().output_tokens, 0);
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
        assert_eq!(h.total_usage().input_tokens, 300);

        assert!(h.rewind_last_turn().is_some());
        assert_eq!(h.total_usage().input_tokens, 100);

        assert!(h.rewind_last_turn().is_some());
        assert_eq!(h.total_usage().input_tokens, 0);
        assert!(h.is_empty());

        assert!(h.rewind_last_turn().is_none());
        assert_eq!(h.total_usage().input_tokens, 0);
    }

    #[test]
    fn rewind_rolls_back_every_call_within_the_turn() {
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
        assert_eq!(h.total_usage().input_tokens, 160);

        h.rewind_last_turn();
        assert_eq!(h.total_usage().input_tokens, 10);
    }

    #[test]
    fn trim_then_rewind_rolls_back_only_remaining_turn() {
        let mut h = History::new();
        h.push(Message::user_text("first"));
        h.push(Message::assistant_text("one"));
        h.record_usage(&usage(100));
        h.push(Message::user_text("second"));
        h.push(Message::assistant_text("two"));
        h.record_usage(&usage(200));

        // Budget trim drops the first turn but keeps the cumulative total.
        h.trim_to_budget(1);
        let roles: Vec<Role> = h.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant]);
        assert_eq!(h.total_usage().input_tokens, 300);

        h.rewind_last_turn();
        assert_eq!(h.total_usage().input_tokens, 100);

        // Nothing left to rewind; the trimmed turn's usage stays in the
        // cumulative total (trim rolls back history, not billing).
        assert!(h.rewind_last_turn().is_none());
        assert_eq!(h.total_usage().input_tokens, 100);
        assert!(h.is_empty());
    }

    #[test]
    fn set_messages_restarts_turn_usage_attribution() {
        let mut h = History::new();
        h.push(Message::user_text("old"));
        h.push(Message::assistant_text("one"));
        h.record_usage(&usage(100));

        h.set_messages(vec![
            Message::user_text("resumed"),
            Message::assistant_text("two"),
        ]);
        assert_eq!(h.total_usage().input_tokens, 0);

        h.record_usage(&usage(300));
        assert_eq!(h.total_usage().input_tokens, 300);

        h.rewind_last_turn();
        assert_eq!(h.total_usage().input_tokens, 0);
        assert!(h.is_empty());
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
}

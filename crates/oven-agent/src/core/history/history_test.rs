use oven_llm::{ContentBlock, Role};

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
        raw_arguments: None,
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
            Record::Thinking { .. } => "thinking",
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
            (
                Record::Thinking {
                    timestamp: t1,
                    duration_ms: d1,
                },
                Record::Thinking {
                    timestamp: t2,
                    duration_ms: d2,
                },
            ) => {
                assert_eq!(t1, t2);
                assert_eq!(d1, d2);
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
fn record_usage_keeps_only_the_latest_response() {
    let mut h = History::new();
    h.push(Message::user_text("hi"));
    h.record_usage(&usage(100));
    assert_eq!(h.last_turn_usage().input_tokens, 100);

    h.record_usage(&usage(150));
    assert_eq!(h.last_turn_usage().input_tokens, 150);
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
fn rewind_rolls_back_the_last_usage_of_the_removed_turn() {
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
    assert_eq!(usage_inputs(&h), vec![10, 50]);

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
    assert_eq!(usage_inputs(&h), vec![100, 75]);

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
fn restore_keeps_the_last_usage_record_of_a_turn() {
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
    assert_eq!(usage_inputs(&h), vec![50]);
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
fn records_from_appends_the_same_bytes_as_a_full_rewrite() {
    let mut h = History::new();
    h.ensure_session_meta("/ws".into());
    let mut persisted = Vec::new();
    let mut cursor = 0;
    let steps = [
        ("first", true),
        ("second", false),
        ("third", true),
        ("fourth", false),
    ];
    for (index, (text, answers)) in steps.iter().enumerate() {
        h.push(Message::user_text(*text));
        if *answers {
            h.push(Message::assistant_text(format!("reply {index}")));
            h.record_usage(&usage(10 * index as u32 + 1));
        }
        persisted.extend(h.records_from(cursor));
        cursor = h.len();
    }
    assert_records_equal(&persisted, &h.records());
    assert!(h.records_from(h.len()).is_empty());
    assert!(h.records_from(h.len() + 10).is_empty());
}

#[test]
fn shared_messages_alias_the_stored_messages() {
    let mut h = History::new();
    h.push(Message::user_text("hi"));
    let snapshot: Vec<_> = h.shared_messages().cloned().collect();
    assert_eq!(snapshot.len(), 1);
    assert!(
        Arc::ptr_eq(&snapshot[0], h.shared_messages().next().unwrap()),
        "a snapshot must alias the stored message, not copy it"
    );
}

#[test]
fn records_never_emit_todo_list() {
    use crate::core::todo::{TodoItem, TodoStatus};

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

#[test]
fn last_turn_duration_uses_message_timestamps() {
    let mut h = History::new();
    h.set_messages_with_records(vec![
        Record::Message {
            timestamp: 1_000,
            message: Message::user_text("q"),
        },
        Record::Message {
            timestamp: 2_500,
            message: Message::assistant_text("a"),
        },
    ]);
    assert_eq!(h.last_user_timestamp(), Some(1_000));
    assert_eq!(h.last_turn_duration_ms(), Some(1_500));
}

#[test]
fn last_turn_duration_none_without_user() {
    let h = History::new();
    assert_eq!(h.last_user_timestamp(), None);
    assert_eq!(h.last_turn_duration_ms(), None);
    assert_eq!(h.elapsed_ms(), 0);
}

#[test]
fn last_turn_duration_zero_when_only_user_message() {
    let mut h = History::new();
    h.set_messages_with_records(vec![Record::Message {
        timestamp: 40,
        message: Message::user_text("q"),
    }]);
    assert_eq!(h.last_turn_duration_ms(), Some(0));
}

#[test]
fn records_roundtrip_preserves_turn_duration() {
    let mut h = History::new();
    h.set_messages_with_records(vec![
        Record::Message {
            timestamp: 10,
            message: Message::user_text("first"),
        },
        Record::Message {
            timestamp: 40,
            message: Message::assistant_text("one"),
        },
        Record::TokenUsage {
            timestamp: 40,
            usage: usage(3),
        },
        Record::Message {
            timestamp: 50,
            message: Message::user_text("second"),
        },
        Record::Message {
            timestamp: 90,
            message: Message::assistant_text("two"),
        },
    ]);
    assert_eq!(h.last_turn_duration_ms(), Some(40));

    let mut restored = History::new();
    restored.set_messages_with_records(h.records());
    assert_eq!(restored.last_turn_duration_ms(), Some(40));
    restored.rewind_last_turn();
    assert_eq!(restored.last_turn_duration_ms(), Some(30));
}

#[test]
fn iter_timed_pairs_messages_with_record_timestamps() {
    let mut h = History::new();
    h.set_messages_with_records(vec![
        Record::Message {
            timestamp: 7,
            message: Message::user_text("q"),
        },
        Record::Message {
            timestamp: 9,
            message: Message::assistant_text("a"),
        },
    ]);
    let timed: Vec<(Role, u64, Option<u64>)> =
        h.iter_timed().map(|(m, ts, th)| (m.role, ts, th)).collect();
    assert_eq!(
        timed,
        vec![(Role::User, 7, None), (Role::Assistant, 9, None)]
    );
}

#[test]
fn thinking_record_roundtrips_on_the_last_message() {
    let mut h = History::new();
    h.set_messages_with_records(vec![
        Record::Message {
            timestamp: 10,
            message: Message::user_text("q"),
        },
        Record::Message {
            timestamp: 40,
            message: Message::assistant(vec![
                ContentBlock::thinking("plan"),
                ContentBlock::text("a"),
            ]),
        },
    ]);
    h.record_thinking(12, 1_500);
    let records = h.records();
    assert_eq!(record_kinds(&records), vec!["msg", "msg", "thinking"]);
    let Record::Thinking {
        timestamp,
        duration_ms,
    } = records[2]
    else {
        panic!("expected thinking record");
    };
    assert_eq!(timestamp, 12);
    assert_eq!(duration_ms, 1_500);

    let mut restored = History::new();
    restored.set_messages_with_records(records);
    let thinking: Vec<Option<u64>> = restored.iter_timed().map(|(_, _, th)| th).collect();
    assert_eq!(thinking, vec![None, Some(1_500)]);
}

#[test]
fn restore_without_thinking_records_leaves_none() {
    let mut h = History::new();
    h.set_messages_with_records(vec![
        Record::Message {
            timestamp: 1,
            message: Message::user_text("q"),
        },
        Record::Message {
            timestamp: 2,
            message: Message::assistant(vec![
                ContentBlock::thinking("plan"),
                ContentBlock::text("a"),
            ]),
        },
    ]);
    let thinking: Vec<Option<u64>> = h.iter_timed().map(|(_, _, th)| th).collect();
    assert_eq!(thinking, vec![None, None]);
    assert!(
        !h.records()
            .iter()
            .any(|r| matches!(r, Record::Thinking { .. }))
    );
}

#[test]
fn record_thinking_skips_zero_duration() {
    let mut h = History::new();
    h.push(Message::assistant_text("a"));
    h.record_thinking(1, 0);
    assert!(
        !h.records()
            .iter()
            .any(|r| matches!(r, Record::Thinking { .. }))
    );
}

#[test]
fn rewind_drops_thinking_with_the_turn() {
    let mut h = History::new();
    h.set_messages_with_records(vec![
        Record::Message {
            timestamp: 1,
            message: Message::user_text("first"),
        },
        Record::Message {
            timestamp: 2,
            message: Message::assistant_text("one"),
        },
        Record::Thinking {
            timestamp: 1,
            duration_ms: 100,
        },
        Record::Message {
            timestamp: 3,
            message: Message::user_text("second"),
        },
        Record::Message {
            timestamp: 4,
            message: Message::assistant_text("two"),
        },
        Record::Thinking {
            timestamp: 3,
            duration_ms: 200,
        },
    ]);
    h.rewind_last_turn();
    let thinking: Vec<Option<u64>> = h.iter_timed().map(|(_, _, th)| th).collect();
    assert_eq!(thinking, vec![None, Some(100)]);
}

#[test]
fn drop_uncommitted_removes_the_open_step_and_restores_usage() {
    const OPEN_STEP_INPUT: u32 = 99;
    let mut history = History::new();
    history.push(Message::user_text("ask"));
    history.note_committed();
    history.drop_uncommitted();
    assert_eq!(
        history.len(),
        1,
        "a step that never landed leaves the prompt"
    );

    history.push(Message::assistant_text("done"));
    history.record_usage(&usage(10));
    history.record_thinking(1, 40);
    history.note_committed();

    history.push(assistant_tools("c2", "hold", serde_json::json!({})));
    history.record_usage(&usage(OPEN_STEP_INPUT));
    history.record_thinking(2, 15);
    history.drop_uncommitted();

    let roles: Vec<Role> = history.iter().map(|message| message.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant]);
    assert!(matches!(&history[1].content[0], ContentBlock::Text { text } if text == "done"));
    assert_eq!(history.last_turn_usage().input_tokens, 10);
    let thinking: Vec<Option<u64>> = history.iter_timed().map(|(_, _, span)| span).collect();
    assert_eq!(thinking, vec![None, Some(40)]);
    let inputs = usage_inputs(&history);
    assert_eq!(inputs, vec![10]);
}

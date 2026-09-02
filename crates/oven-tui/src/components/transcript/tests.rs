use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use oven_app::{
    AgentEvent, AppEvent, LocalShell, ShellEvent, StreamEvent, ToolCallId, ToolEvent, ToolResult,
    TurnEvent, present_tool,
};
use oven_llm::{ContentBlock, Message};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Rect;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::component::{Action, Component, KeyResult, State};
use super::super::theme;
use super::kinds::{LINE_PREFIX_WIDTH, LineKind, SEPARATOR_GLYPH, THINKING_LABEL, THOUGHT_LABEL};
use super::selection::{extract_line_range, highlight_line, slice_cols};

use super::widget::Transcript;
use super::wrap::{
    MAX_RESULT_LINES, MAX_SHELL_DISPLAY_LINES, apply_thinking_shimmer, format_lines, tail_lines,
    truncate_result,
};

fn wide(t: &mut Transcript) {
    t.width = 80;
    t.rewrap_all();
}

fn fill(t: &mut Transcript, n: usize) {
    for i in 0..n {
        t.push_row(LineKind::Text, &format!("line {i}"));
    }
}

fn line_body(kind: LineKind, text: &str) -> String {
    format_lines(kind, text)[0]
        .spans
        .iter()
        .skip(1)
        .map(|s| s.content.as_ref())
        .collect()
}

#[test]
fn thinking_format_hides_content() {
    assert_eq!(line_body(LineKind::Thinking, "secret plan"), THOUGHT_LABEL);
    assert_eq!(
        line_body(LineKind::Thinking, THINKING_LABEL),
        THINKING_LABEL
    );
}

#[test]
fn thinking_shimmer_preserves_label_and_shifts() {
    let line = format_lines(LineKind::Thinking, THINKING_LABEL)
        .pop()
        .unwrap();
    let a = apply_thinking_shimmer(&line, 0.0);
    let b = apply_thinking_shimmer(&line, 0.5);
    let body: String = a.spans.iter().skip(1).map(|s| s.content.as_ref()).collect();
    assert_eq!(body, THINKING_LABEL);
    assert_eq!(a.spans.len(), 1 + THINKING_LABEL.chars().count());
    assert_ne!(a.spans[1].style.fg, b.spans[1].style.fg);
}

#[test]
fn truncate_result_caps_lines() {
    let text = (0..10)
        .map(|i| format!("l{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = truncate_result(&text);
    assert!(out.ends_with("… 4 more"));
    assert_eq!(out.lines().count(), MAX_RESULT_LINES + 1);
}

#[test]
fn all_gutters_are_two_wide() {
    let kinds = [
        LineKind::User,
        LineKind::Shell,
        LineKind::Thinking,
        LineKind::Text,
        LineKind::Tool,
        LineKind::Diff,
        LineKind::ToolResult(true),
        LineKind::ToolResult(false),
        LineKind::ShellResult(true),
        LineKind::ShellResult(false),
        LineKind::Error,
        LineKind::System,
        LineKind::Separator,
    ];
    for kind in kinds {
        assert_eq!(kind.gutter().width(), LINE_PREFIX_WIDTH);
        for ch in kind.gutter().chars() {
            assert_eq!(ch.width().unwrap_or(0), 1, "{ch:?} must be single-width");
        }
    }
    assert_eq!(SEPARATOR_GLYPH.width().unwrap_or(0), 1);
}

#[test]
fn assistant_gutter_is_bullet() {
    let lines = format_lines(LineKind::Text, "hi");
    assert_eq!(lines[0].spans[0].content.as_ref(), LineKind::Text.gutter());
}

#[test]
fn shell_command_gutter_is_dollar() {
    let lines = format_lines(LineKind::Shell, "ls");
    assert_eq!(lines[0].spans[0].content.as_ref(), LineKind::Shell.gutter());
    assert_eq!(lines[0].spans[0].style.fg, theme::shell().fg);
    assert_eq!(lines[0].spans[1].content.as_ref(), "ls");
    assert_eq!(lines[0].spans[1].style.fg, theme::shell().fg);
}

#[test]
fn regular_text_keeps_default_body_style() {
    let lines = format_lines(LineKind::Text, "hello");
    assert_eq!(lines[0].spans[1].style, ratatui::style::Style::default());
}

#[test]
fn diff_lines_have_add_remove_backgrounds() {
    let lines = format_lines(LineKind::Diff, "Edit file.txt\n- old\n+ new");
    assert_eq!(
        lines[1].spans[1].style.bg,
        Some(ratatui::style::Color::LightRed)
    );
    assert_eq!(
        lines[2].spans[1].style.bg,
        Some(ratatui::style::Color::LightGreen)
    );
}

#[test]
fn tool_result_gutters() {
    let ok = format_lines(LineKind::ToolResult(true), "out");
    assert_eq!(ok[0].spans[0].content.as_ref(), "  ");
    let fail = format_lines(LineKind::ToolResult(false), "boom");
    assert_eq!(fail[0].spans[0].content.as_ref(), "  ");
}

#[test]
fn streaming_does_not_yank_scrolled_view() {
    let mut t = Transcript::new();
    wide(&mut t);
    t.view_height = 3;
    fill(&mut t, 10);
    t.scroll_up(2);
    let anchored = t.top;
    assert!(!t.pinned);
    t.push_stream(LineKind::Text, "more");
    assert_eq!(t.top, anchored, "reading position must not move");
    assert!(!t.pinned);
}

#[test]
fn streaming_follows_when_pinned() {
    let mut t = Transcript::new();
    wide(&mut t);
    t.view_height = 3;
    fill(&mut t, 10);
    assert!(t.pinned);
    t.push_stream(LineKind::Text, "more");
    assert!(t.pinned);
    t.rewrap_stream();
    assert_eq!(
        t.current_top(),
        t.total_lines().saturating_sub(t.view_height)
    );
}

#[test]
fn scroll_down_returns_to_bottom() {
    let mut t = Transcript::new();
    wide(&mut t);
    t.view_height = 3;
    fill(&mut t, 10);
    t.scroll_up(5);
    assert!(!t.pinned);
    t.scroll_down(5);
    assert!(t.pinned);
}

#[test]
fn tool_end_updates_summary_without_result_row() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(
        1,
        "bash",
        serde_json::json!({ "command": "ls" }),
    ));
    t.on_event(&tool_end(1, false, "boom\n"));
    let row = t.rows.last().unwrap();
    assert_eq!(row.kind, LineKind::Tool);
    assert_eq!(row.text, "Ran ls · 1 failed");
    assert_eq!(t.rows.len(), 1);
}

#[test]
fn empty_ok_tool_end_renders_nothing() {
    let mut t = Transcript::new();
    let n = t.rows.len();
    t.on_event(&tool_end(1, true, ""));
    assert_eq!(t.rows.len(), n);
}

#[test]
fn seed_renders_persisted_messages() {
    let mut t = Transcript::new();
    let messages = vec![
        Message::user_text("hello"),
        Message::assistant(vec![
            ContentBlock::Thinking {
                thinking: "hmm".into(),
            },
            ContentBlock::Text {
                text: "hi there".into(),
            },
            ContentBlock::ToolUse {
                id: "c1".into(),
                name: "bash".into(),
                input: serde_json::json!({ "command": "ls" }),
            },
        ]),
        Message::tool_result("c1", "done", false),
        Message::user(vec![
            ContentBlock::Text {
                text: "thanks".into(),
            },
            ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: vec![ContentBlock::text("boom")],
                is_error: true,
            },
        ]),
    ];
    t.seed(&messages);

    let kinds: Vec<LineKind> = t.rows.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        vec![
            LineKind::User,
            LineKind::Thinking,
            LineKind::Text,
            LineKind::Tool,
            LineKind::User,
        ]
    );
    assert_eq!(t.rows[0].text, "hello");
    assert_eq!(t.rows[1].text, THOUGHT_LABEL);
    assert_eq!(t.rows[3].text, "Ran ls");
}

#[test]
fn seed_empty_history_is_empty() {
    let mut t = Transcript::new();
    t.seed(&[]);
    assert!(t.rows.is_empty());
}

#[test]
fn reply_event_does_not_append_to_transcript() {
    let mut t = Transcript::new();
    t.push_user("/model");
    let n = t.rows.len();
    t.on_event(&AppEvent::notification("current model: gpt-4o"));
    assert_eq!(t.rows.len(), n);
}

#[test]
fn seed_mirrors_empty_tool_result_handling() {
    let mut t = Transcript::new();
    t.seed(&[Message::tool_result("c1", "", false)]);
    assert!(t.rows.is_empty(), "orphan ok result is dropped");

    let mut t = Transcript::new();
    t.seed(&[Message::tool_result("c1", "", true)]);
    assert!(t.rows.is_empty(), "orphan error result is dropped");
}

fn agent(event: AgentEvent) -> AppEvent {
    AppEvent::agent(event)
}

fn tool_start(call_id: u64, name: &str, input: serde_json::Value) -> AppEvent {
    agent(AgentEvent::Tool(ToolEvent::Started {
        call_id: ToolCallId(call_id),
        name: name.into(),
        view: present_tool(name, &input),
    }))
}

fn tool_end(call_id: u64, ok: bool, output: &str) -> AppEvent {
    agent(AgentEvent::Tool(ToolEvent::Finished {
        call_id: ToolCallId(call_id),
        result: if ok {
            ToolResult::Success {
                output: output.into(),
            }
        } else {
            ToolResult::Failed {
                error: output.into(),
                output: Some(output.into()),
            }
        },
    }))
}

fn text_delta(text: &str) -> AppEvent {
    agent(AgentEvent::Stream(StreamEvent::TextDelta {
        text: text.into(),
    }))
}

fn thinking(text: &str) -> AppEvent {
    agent(AgentEvent::Stream(StreamEvent::ThinkingDelta {
        text: text.into(),
    }))
}

fn completed() -> AppEvent {
    agent(AgentEvent::Turn(TurnEvent::Completed {
        usage: oven_llm::Usage::default(),
    }))
}

fn cancelled() -> AppEvent {
    agent(AgentEvent::Turn(TurnEvent::Cancelled))
}

fn kinds_of(t: &Transcript) -> Vec<LineKind> {
    t.rows.iter().map(|r| r.kind).collect()
}

#[test]
fn done_appends_separator_after_answer() {
    let mut t = Transcript::new();
    t.push_user("q");
    t.on_event(&text_delta("a"));
    t.on_event(&completed());
    assert_eq!(
        kinds_of(&t),
        vec![LineKind::User, LineKind::Text, LineKind::Separator]
    );
}

#[test]
fn tool_end_does_not_append_separator() {
    let mut t = Transcript::new();
    t.push_user("q");
    t.on_event(&tool_start(
        1,
        "bash",
        serde_json::json!({ "command": "ls" }),
    ));
    t.on_event(&tool_end(1, true, "done"));
    assert_eq!(kinds_of(&t), vec![LineKind::User, LineKind::Tool,]);
    assert_eq!(t.rows[1].text, "Ran ls");
}

#[test]
fn separator_comes_after_tool_followup_not_between() {
    let mut t = Transcript::new();
    t.push_user("q");
    t.on_event(&tool_start(
        1,
        "bash",
        serde_json::json!({ "command": "ls" }),
    ));
    t.on_event(&tool_end(1, true, "done"));
    t.on_event(&text_delta("ok"));
    t.on_event(&completed());
    assert_eq!(
        kinds_of(&t),
        vec![
            LineKind::User,
            LineKind::Tool,
            LineKind::Text,
            LineKind::Separator,
        ]
    );
}

#[test]
fn cancelled_appends_separator() {
    let mut t = Transcript::new();
    t.push_user("q");
    t.on_event(&cancelled());
    assert_eq!(
        kinds_of(&t),
        vec![LineKind::User, LineKind::System, LineKind::Separator]
    );
}

#[test]
fn thinking_delta_shows_label_not_content() {
    let mut t = Transcript::new();
    t.on_event(&thinking("secret chain of thought"));
    t.on_event(&thinking(" more secrets"));
    t.on_event(&text_delta("answer"));
    t.on_event(&completed());
    assert_eq!(
        kinds_of(&t),
        vec![LineKind::Thinking, LineKind::Text, LineKind::Separator]
    );
    assert_eq!(t.rows[0].text, THOUGHT_LABEL);
    assert_eq!(t.rows[1].text, "answer");
    assert!(t.rows.iter().all(|r| !r.text.contains("secret")));
}

#[test]
fn seed_collapses_consecutive_thinking() {
    let mut t = Transcript::new();
    t.seed(&[Message::assistant(vec![
        ContentBlock::Thinking {
            thinking: "one".into(),
        },
        ContentBlock::Thinking {
            thinking: "two".into(),
        },
        ContentBlock::Text { text: "hi".into() },
    ])]);
    assert_eq!(
        kinds_of(&t),
        vec![LineKind::Thinking, LineKind::Text, LineKind::Separator]
    );
    assert_eq!(t.rows[0].text, THOUGHT_LABEL);
}

#[test]
fn seed_separates_complete_turns_not_tool_followup() {
    let mut t = Transcript::new();
    t.seed(&[
        Message::user_text("one"),
        Message::assistant(vec![ContentBlock::Text {
            text: "first".into(),
        }]),
        Message::user_text("two"),
        Message::assistant(vec![
            ContentBlock::Text {
                text: "checking".into(),
            },
            ContentBlock::ToolUse {
                id: "c1".into(),
                name: "bash".into(),
                input: serde_json::json!({ "command": "ls" }),
            },
        ]),
        Message::tool_result("c1", "out", false),
        Message::assistant(vec![ContentBlock::Text {
            text: "second".into(),
        }]),
    ]);
    assert_eq!(
        kinds_of(&t),
        vec![
            LineKind::User,
            LineKind::Text,
            LineKind::Separator,
            LineKind::User,
            LineKind::Text,
            LineKind::Tool,
            LineKind::Text,
            LineKind::Separator,
        ]
    );
}

#[test]
fn live_tools_aggregate_counts_and_failures() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(
        1,
        "bash",
        serde_json::json!({ "command": "ls" }),
    ));
    t.on_event(&tool_end(1, true, "ok"));
    t.on_event(&tool_start(
        2,
        "bash",
        serde_json::json!({ "command": "pwd" }),
    ));
    t.on_event(&tool_end(2, false, "boom"));
    t.on_event(&tool_start(
        3,
        "file_read",
        serde_json::json!({ "path": "a" }),
    ));
    t.on_event(&tool_end(3, true, "hi"));
    assert_eq!(kinds_of(&t), vec![LineKind::Tool]);
    assert_eq!(t.rows[0].text, "Ran ×2 (ls · pwd) · Read a · 1 failed");
}

#[test]
fn live_tool_end_rewrites_same_summary_row() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(
        1,
        "bash",
        serde_json::json!({ "command": "ls" }),
    ));
    assert_eq!(t.rows.len(), 1);
    assert_eq!(t.rows[0].text, "Ran ls");
    t.on_event(&tool_end(1, false, "boom"));
    assert_eq!(t.rows.len(), 1);
    assert_eq!(t.rows[0].kind, LineKind::Tool);
    assert_eq!(t.rows[0].text, "Ran ls · 1 failed");
}

fn todo_input() -> serde_json::Value {
    serde_json::json!({
        "todos": [{"id": "a", "content": "one", "status": "pending"}]
    })
}

#[test]
fn todo_write_keeps_detail_and_result() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(10, "todo_write", todo_input()));
    t.on_event(&tool_end(10, true, "updated"));
    assert_eq!(
        kinds_of(&t),
        vec![LineKind::Tool, LineKind::ToolResult(true)]
    );
    assert_eq!(
        t.rows[0].text,
        "todo_write · 1 todos (0 in_progress, 0 completed)"
    );
    assert_eq!(t.rows[1].text, "updated");
}

#[test]
fn todo_write_splits_tool_bursts() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(
        1,
        "bash",
        serde_json::json!({ "command": "ls" }),
    ));
    t.on_event(&tool_end(1, true, "ok"));
    t.on_event(&tool_start(10, "todo_write", todo_input()));
    t.on_event(&tool_end(10, true, "updated"));
    t.on_event(&tool_start(
        2,
        "bash",
        serde_json::json!({ "command": "pwd" }),
    ));
    t.on_event(&tool_end(2, true, "ok"));
    assert_eq!(
        kinds_of(&t),
        vec![
            LineKind::Tool,
            LineKind::Tool,
            LineKind::ToolResult(true),
            LineKind::Tool,
        ]
    );
    assert_eq!(t.rows[0].text, "Ran ls");
    assert_eq!(
        t.rows[1].text,
        "todo_write · 1 todos (0 in_progress, 0 completed)"
    );
    assert_eq!(t.rows[3].text, "Ran pwd");
}

#[test]
fn restored_tool_trajectory_matches_live_presentation() {
    let grep = serde_json::json!({
        "pattern": "ToolEvent",
        "path": "crates",
        "include": "*.rs"
    });
    let glob = serde_json::json!({ "pattern": "**/*.rs", "path": "crates" });

    let mut live = Transcript::new();
    live.on_event(&tool_start(1, "grep", grep.clone()));
    live.on_event(&tool_end(1, true, "event.rs:1:ToolEvent"));
    live.on_event(&tool_start(2, "glob", glob.clone()));
    live.on_event(&tool_end(2, true, "crates/oven-agent/src/agent.rs"));

    let mut restored = Transcript::new();
    restored.seed(&[
        Message::assistant(vec![
            ContentBlock::ToolUse {
                id: "c1".into(),
                name: "grep".into(),
                input: grep,
            },
            ContentBlock::ToolUse {
                id: "c2".into(),
                name: "glob".into(),
                input: glob,
            },
        ]),
        Message::tool_result("c1", "event.rs:1:ToolEvent", false),
        Message::tool_result("c2", "crates/oven-agent/src/agent.rs", false),
    ]);

    assert_eq!(kinds_of(&live), kinds_of(&restored));
    let live_rows: Vec<_> = live.rows.iter().map(|row| row.text.as_str()).collect();
    let restored_rows: Vec<_> = restored.rows.iter().map(|row| row.text.as_str()).collect();
    assert_eq!(live_rows, restored_rows);
    assert_eq!(
        live_rows,
        vec!["Search ToolEvent in crates (*.rs) · Find **/*.rs in crates"]
    );
}

#[test]
fn seed_todo_write_keeps_result() {
    let mut t = Transcript::new();
    t.seed(&[
        Message::assistant(vec![ContentBlock::ToolUse {
            id: "t1".into(),
            name: "todo_write".into(),
            input: todo_input(),
        }]),
        Message::tool_result("t1", "updated", false),
    ]);
    assert_eq!(
        kinds_of(&t),
        vec![LineKind::Tool, LineKind::ToolResult(false)]
    );
    assert_eq!(
        t.rows[0].text,
        "todo_write · 1 todos (0 in_progress, 0 completed)"
    );
    assert_eq!(t.rows[1].text, "updated");
}

#[test]
fn seed_failed_tool_counts_without_result() {
    let mut t = Transcript::new();
    t.seed(&[
        Message::assistant(vec![
            ContentBlock::ToolUse {
                id: "c1".into(),
                name: "bash".into(),
                input: serde_json::json!({ "command": "ls" }),
            },
            ContentBlock::ToolUse {
                id: "c2".into(),
                name: "bash".into(),
                input: serde_json::json!({ "command": "pwd" }),
            },
            ContentBlock::ToolUse {
                id: "c3".into(),
                name: "file_read".into(),
                input: serde_json::json!({ "path": "a" }),
            },
        ]),
        Message::tool_result("c1", "ok", false),
        Message::tool_result("c2", "boom", true),
        Message::tool_result("c3", "hi", false),
    ]);
    assert_eq!(kinds_of(&t), vec![LineKind::Tool]);
    assert_eq!(t.rows[0].text, "Ran ×2 (ls · pwd) · Read a · 1 failed");
}

#[test]
fn separator_renders_full_width_rule() {
    let mut t = Transcript::new();
    wide(&mut t);
    t.push_user("q");
    t.on_event(&text_delta("a"));
    t.on_event(&completed());
    let last = t.wrapped.last().expect("wrapped separator");
    let text: String = last.spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text.chars().filter(|c| *c == SEPARATOR_GLYPH).count(), 80);
}

#[test]
fn last_user_text_returns_most_recent_user_row() {
    let mut t = Transcript::new();
    t.push_user("first");
    t.push_row(LineKind::Text, "one");
    t.push_user("second");
    assert_eq!(t.last_user_text().as_deref(), Some("second"));
}

#[test]
fn last_user_text_none_without_user_rows() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "one");
    assert_eq!(t.last_user_text(), None);
}

#[test]
fn last_user_text_rewinds_shell_as_bang() {
    let mut t = Transcript::new();
    t.push_shell_command("ls -la");
    assert_eq!(t.last_user_text().as_deref(), Some("! ls -la"));
}

#[test]
fn replace_from_rebuilds_rows() {
    let mut t = Transcript::new();
    t.push_user("old");
    t.replace_from(&[Message::user_text("resumed")]);
    assert_eq!(t.rows[0].text, "resumed");
    assert_eq!(t.rows.len(), 1);
}

#[test]
fn page_keys_scroll_by_viewport() {
    use crossterm::event::KeyModifiers;

    let mut t = Transcript::new();
    wide(&mut t);
    t.view_height = 5;
    fill(&mut t, 20);
    let bottom = t.total_lines().saturating_sub(5);
    let page = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
    t.handle_key(page, &State::new());
    assert!(!t.pinned);
    assert_eq!(t.top, bottom.saturating_sub(5));
    t.handle_key(
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
        &State::new(),
    );
    assert!(t.pinned);
}

fn ready(t: &mut Transcript, area: Rect) {
    t.area = area;
    t.width = area.width as usize;
    t.view_height = area.height as usize;
    t.rewrap_all();
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    }
}

#[test]
fn slice_cols_by_display_width() {
    assert_eq!(slice_cols("hello", 1, 4), "ell");
    assert_eq!(slice_cols("你好", 0, 2), "你");
    assert_eq!(slice_cols("你好", 2, 4), "好");
    assert_eq!(slice_cols("hello", 3, 3), "");
}

#[test]
fn extract_skips_gutter() {
    let line = format_lines(LineKind::Text, "hello").pop().unwrap();
    assert_eq!(extract_line_range(&line, 0, 7), "hello");
    assert_eq!(extract_line_range(&line, 2, 7), "hello");
    assert_eq!(extract_line_range(&line, 2, 5), "hel");
    assert_eq!(extract_line_range(&line, 0, 2), "");
}

#[test]
fn mouse_drag_selects_body_without_gutter() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "hello");
    ready(&mut t, Rect::new(0, 0, 80, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 2, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 7, 0),
        &State::new(),
    );
    assert_eq!(t.selected_text().as_deref(), Some("hello"));
}

#[test]
fn mouse_drag_reverse_selects_same_text() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "hello");
    ready(&mut t, Rect::new(0, 0, 80, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 7, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 2, 0),
        &State::new(),
    );
    assert_eq!(t.selected_text().as_deref(), Some("hello"));
}

#[test]
fn mouse_drag_selects_across_rows() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "hello");
    t.push_row(LineKind::Text, "world");
    ready(&mut t, Rect::new(0, 0, 80, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 2, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 7, 2),
        &State::new(),
    );
    assert_eq!(t.selected_text().as_deref(), Some("hello\n\nworld"));
}

#[test]
fn mouse_click_without_drag_is_empty() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "hello");
    ready(&mut t, Rect::new(0, 0, 80, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 3, 0),
        &State::new(),
    );
    assert_eq!(t.selected_text(), None);
    let up = t.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), 3, 0),
        &State::new(),
    );
    assert!(matches!(up, KeyResult::Handled));
    assert!(t.select_anchor.is_none());
    assert!(!t.dragging);
}

#[test]
fn mouse_up_after_selection_emits_copied_reply() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "hello");
    ready(&mut t, Rect::new(0, 0, 80, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 2, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 7, 0),
        &State::new(),
    );
    let up = t.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), 7, 0),
        &State::new(),
    );
    assert!(matches!(up, KeyResult::Action(Action::Notify(text)) if text == "Copied!"));
    assert_eq!(t.selected_text().as_deref(), Some("hello"));
}

#[test]
fn mouse_selects_wide_chars() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "你好");
    ready(&mut t, Rect::new(0, 0, 80, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 2, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 4, 0),
        &State::new(),
    );
    assert_eq!(t.selected_text().as_deref(), Some("你"));
}

#[test]
fn mouse_selects_wrapped_lines() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "abcdefgh");
    ready(&mut t, Rect::new(0, 0, 6, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 2, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 6, 1),
        &State::new(),
    );
    assert_eq!(t.selected_text().as_deref(), Some("abcd\nefgh"));
}

#[test]
fn mouse_down_outside_is_ignored() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "hello");
    ready(&mut t, Rect::new(0, 0, 80, 5));
    let r = t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 0, 20),
        &State::new(),
    );
    assert!(matches!(r, KeyResult::Ignored));
    assert!(t.select_anchor.is_none());
}

#[test]
fn mouse_drag_outside_extends_to_end() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "hello");
    ready(&mut t, Rect::new(0, 0, 80, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 2, 0),
        &State::new(),
    );
    let r = t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 0, 20),
        &State::new(),
    );
    assert!(matches!(r, KeyResult::Handled));
    assert_eq!(t.selected_text().as_deref(), Some("hello"));
}

#[test]
fn highlight_line_marks_range() {
    let line = format_lines(LineKind::Text, "hello").pop().unwrap();
    let hi = highlight_line(&line, 2, 7);
    assert_eq!(hi.spans.len(), 2);
    assert_eq!(hi.spans[0].content.as_ref(), LineKind::Text.gutter());
    assert_eq!(hi.spans[1].content.as_ref(), "hello");
    assert_eq!(hi.spans[1].style, theme::selection());
}

#[test]
fn stream_text_caret_blinks() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut t = Transcript::new();
    t.push_stream(LineKind::Text, "hello");
    let mut state = State::new();
    let backend = TestBackend::new(20, 2);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| t.draw(f, f.area(), &state)).unwrap();
    let on: String = {
        let buf = terminal.backend().buffer();
        (0..20).map(|x| buf[(x, 0)].symbol().to_string()).collect()
    };
    assert!(on.contains("hello"), "{on:?}");
    assert!(on.contains("▊"), "caret on at frame 0: {on:?}");

    state.frame = 5;
    terminal.draw(|f| t.draw(f, f.area(), &state)).unwrap();
    let off: String = {
        let buf = terminal.backend().buffer();
        (0..20).map(|x| buf[(x, 0)].symbol().to_string()).collect()
    };
    assert!(off.contains("hello"), "{off:?}");
    assert!(!off.contains("▊"), "caret off at frame 5: {off:?}");
}

#[test]
fn scrolled_stream_does_not_draw_caret_on_history() {
    let mut t = Transcript::new();
    for i in 0..10 {
        t.push_row(LineKind::Text, &format!("line {i}"));
    }
    t.push_stream(LineKind::Text, "hello");
    let backend = TestBackend::new(20, 3);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| t.draw(f, f.area(), &State::new()))
        .unwrap();
    t.scroll_up(2);
    terminal
        .draw(|f| t.draw(f, f.area(), &State::new()))
        .unwrap();

    let visible: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(
        !visible.contains("▊"),
        "history must not show stream caret: {visible:?}"
    );
}

#[test]
fn thinking_stream_does_not_draw_text_caret() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut t = Transcript::new();
    t.push_stream(LineKind::Thinking, THINKING_LABEL);
    let backend = TestBackend::new(20, 2);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| t.draw(f, f.area(), &State::new()))
        .unwrap();
    let row: String = {
        let buf = terminal.backend().buffer();
        (0..20).map(|x| buf[(x, 0)].symbol().to_string()).collect()
    };
    assert!(!row.contains("▊"), "{row:?}");
}

#[test]
fn draw_repaints_every_cell_after_shorter_cjk_line() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "你好世界你好世界");
    let backend = TestBackend::new(20, 3);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| t.draw(f, f.area(), &State::new()))
        .unwrap();

    t.reset();
    t.push_row(LineKind::Text, "好");
    let frame = terminal
        .draw(|f| t.draw(f, f.area(), &State::new()))
        .unwrap();
    let row: String = (0..20).map(|x| frame.buffer[(x, 0)].symbol()).collect();
    assert!(row.starts_with("∙ 好"), "{row:?}");
    assert!(
        row[row.find('好').unwrap() + '好'.len_utf8()..]
            .chars()
            .all(|c| c == ' '),
        "shorter CJK line must not leave previous glyphs: {row:?}"
    );
    for x in 0..20 {
        assert_eq!(
            frame.buffer[(x, 0)].diff_option,
            CellDiffOption::AlwaysUpdate
        );
    }
}

#[test]
fn replace_from_clears_selection() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "hello");
    ready(&mut t, Rect::new(0, 0, 80, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 2, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 7, 0),
        &State::new(),
    );
    assert!(t.selected_text().is_some());
    t.replace_from(&[Message::user_text("resumed")]);
    assert!(t.select_anchor.is_none());
    assert!(!t.dragging);
}

#[test]
fn tail_lines_keeps_last_max() {
    let text = (0..150)
        .map(|i| format!("l{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = tail_lines(&text, MAX_SHELL_DISPLAY_LINES);
    assert!(out.starts_with("… 50 earlier lines"));
    assert!(out.contains("l50"));
    assert!(out.ends_with("l149"));
    assert!(!out.contains("l0\n"));
    assert_eq!(out.lines().count(), MAX_SHELL_DISPLAY_LINES + 1);
}

#[test]
fn seed_shell_envelope_renders_command_and_output() {
    let mut t = Transcript::new();
    let msg = LocalShell {
        command: "ls".into(),
        exit_code: Some(0),
        output: "a.rs\nb.rs".into(),
        error: None,
    }
    .to_string();
    t.seed(&[Message::user_text(msg)]);
    assert_eq!(t.rows[0].kind, LineKind::Shell);
    assert_eq!(t.rows[0].text, "ls");
    assert_eq!(t.rows[1].kind, LineKind::ShellResult(true));
    assert_eq!(t.rows[1].text, "a.rs\nb.rs");
    assert_eq!(t.last_user_text().as_deref(), Some("! ls"));
}

#[test]
fn seed_does_not_treat_bang_user_text_as_shell() {
    let mut t = Transcript::new();
    t.seed(&[Message::user_text("! ls")]);
    assert_eq!(t.rows[0].kind, LineKind::User);
    assert_eq!(t.rows[0].text, "! ls");
    assert_eq!(t.rows.len(), 1);
}

#[test]
fn seed_nonzero_exit_is_failed_result() {
    let mut t = Transcript::new();
    let msg = LocalShell {
        command: "false".into(),
        exit_code: Some(1),
        output: "[exit code: 1]".into(),
        error: None,
    }
    .to_string();
    t.seed(&[Message::user_text(msg)]);
    assert_eq!(t.rows[0].kind, LineKind::Shell);
    assert_eq!(t.rows[1].kind, LineKind::ShellResult(false));
    assert_eq!(t.rows[1].text, "[exit code: 1]");
}

#[test]
fn seed_shell_envelope_does_not_show_raw_xml() {
    let mut t = Transcript::new();
    let msg = LocalShell {
        command: "echo hi".into(),
        exit_code: Some(0),
        output: "hi".into(),
        error: None,
    }
    .to_string();
    t.seed(&[Message::user_text(msg)]);
    assert!(!t.rows.iter().any(|r| r.text.contains("<local-shell>")));
}

#[test]
fn shell_finished_event_appends_tailed_output() {
    let mut t = Transcript::new();
    t.push_shell_command("ls");
    let output = (0..150)
        .map(|i| format!("l{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    t.on_event(&AppEvent::shell(ShellEvent::Finished {
        command: "ls".into(),
        output,
        exit_code: 0,
    }));
    assert_eq!(t.rows[0].kind, LineKind::Shell);
    assert_eq!(t.rows[1].kind, LineKind::ShellResult(true));
    assert!(t.rows[1].text.starts_with("… 50 earlier lines"));
    assert!(t.rows[1].text.ends_with("l149"));
}

#[test]
fn shell_failed_event_is_error_result() {
    let mut t = Transcript::new();
    t.push_shell_command("sleep 60");
    t.on_event(&AppEvent::shell(ShellEvent::Failed {
        command: "sleep 60".into(),
        error: "cancelled".into(),
        output: String::new(),
    }));
    assert_eq!(t.rows[0].kind, LineKind::Shell);
    assert_eq!(t.rows[1].kind, LineKind::ShellResult(false));
    assert_eq!(t.rows[1].text, "cancelled");
}

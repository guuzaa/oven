use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use oven_app::{
    AgentEvent, AppEvent, ApprovalRequestId, LocalShell, LoopLimitRequestId, ShellEvent,
    StreamEvent, ToolCallId, ToolEvent, ToolResult, TurnEvent, present_tool,
};
use oven_llm::{ContentBlock, Message};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::Rect;
use ratatui::text::Line;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::component::{Action, Component, KeyResult, State};
use super::super::theme;
use super::collapsible::Section;
use super::kinds::{COLLAPSED_MARKER, LINE_INDENT, LINE_PREFIX_WIDTH, LineKind, MESSAGE_INDENT};
use super::selection::{extract_line_range, highlight_line, slice_cols};
use super::wrap::{THINKING_LABEL, THOUGHT_LABEL};

use super::widget::{LOOP_LIMIT_REACHED, Transcript};
use super::wrap::{
    MAX_LIVE_BODY_LINES, MAX_SHELL_DISPLAY_LINES, RESULT_LABEL, apply_thinking_shimmer,
    format_lines, format_thought, line_display_width, tail_lines,
};

const THOUGHT_0: &str = "Thought for 0s";
const THOUGHT_1_5S: &str = "Thought for 1.5s";
const THOUGHT_1M_1S: &str = "Thought for 1m 1s";

fn wide(t: &mut Transcript) {
    t.area.width = 80;
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
fn thinking_header_has_no_gutter() {
    let mut t = Transcript::new();
    t.on_event(&thinking("secret chain"));
    t.on_event(&text_delta("answer"));
    wide(&mut t);
    let header = line_text(&t.wrapped[0]);
    let marker = format!("{MESSAGE_INDENT}{COLLAPSED_MARKER}");
    assert!(header.starts_with(&marker), "{header:?}");
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
    ];
    for kind in kinds {
        assert_eq!(kind.gutter().width(), LINE_PREFIX_WIDTH);
        for ch in kind.gutter().chars() {
            assert_eq!(ch.width().unwrap_or(0), 1, "{ch:?} must be single-width");
        }
    }
}

#[test]
fn assistant_gutter_is_bullet() {
    let lines = format_lines(LineKind::Text, "hi");
    assert_eq!(lines[0].spans[0].content.as_ref(), " ∙ ");
}

#[test]
fn assistant_gutter_is_drawn_once() {
    let lines = format_lines(LineKind::Text, "first\nsecond");
    assert_eq!(lines[0].spans[0].content.as_ref(), " ∙ ");
    assert_eq!(lines[1].spans[0].content.as_ref(), "   ");
    assert_eq!(
        lines[0].spans[0].content.width(),
        lines[1].spans[0].content.width()
    );
}

#[test]
fn wrapped_assistant_gutter_is_drawn_once() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "abcdefgh");
    ready(&mut t, Rect::new(0, 0, 7, 5));
    let rows: Vec<String> = t.wrapped.iter().map(line_text).collect();
    assert_eq!(rows, vec![" ∙ abcd", "   efgh"]);
}

#[test]
fn wrapped_streamed_assistant_gutter_is_drawn_once() {
    const WIDTH: u16 = 7;
    let mut t = Transcript::new();
    t.push_stream(LineKind::Text, "abcdefgh");
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, 3)).unwrap();
    terminal
        .draw(|f| t.draw(f, f.area(), &State::new()))
        .unwrap();
    let buf = terminal.backend().buffer();
    let row = |y: u16| (0..WIDTH).map(|x| buf[(x, y)].symbol()).collect::<String>();
    assert!(row(0).starts_with(" ∙ abcd"), "{:?}", row(0));
    assert!(row(1).starts_with("   efgh"), "{:?}", row(1));
}

#[test]
fn wrapped_shell_gutter_repeats() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Shell, "abcdefgh");
    ready(&mut t, Rect::new(0, 0, 7, 5));
    let rows: Vec<String> = t.wrapped.iter().map(line_text).collect();
    assert_eq!(rows, vec![" $ abcd", " $ efgh"]);
}

#[test]
fn non_user_rows_share_one_message_indent() {
    let mut t = Transcript::new();
    for (kind, text) in [
        (LineKind::Text, "assistant text that is long enough to wrap"),
        (LineKind::Shell, "$ command that is long enough to wrap"),
        (LineKind::System, "system note"),
        (LineKind::Error, "failure note"),
        (LineKind::Tool, "tool summary"),
        (LineKind::ToolResult(false), "tool output"),
        (LineKind::Thinking, THOUGHT_LABEL),
    ] {
        t.push_row(kind, text);
    }
    for row in &mut t.rows {
        if let Some(collapsible) = row.collapsible.as_mut() {
            collapsible.toggle();
        }
    }
    ready(&mut t, Rect::new(0, 0, 20, 40));
    let indent = MESSAGE_INDENT.width() + LINE_PREFIX_WIDTH;
    for line in &t.wrapped {
        let text = line_text(line);
        if text.trim().is_empty() {
            continue;
        }
        let prefix = line.spans.first().map_or("", |s| s.content.as_ref());
        assert_eq!(
            prefix.width(),
            indent,
            "{:?} must indent every non-user row by {indent}",
            text
        );
    }
}

#[test]
fn shell_command_gutter_is_dollar() {
    let lines = format_lines(LineKind::Shell, "ls");
    assert_eq!(lines[0].spans[0].content.as_ref(), " $ ");
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
    assert_eq!(ok[0].spans[0].content.as_ref(), "   ");
    let fail = format_lines(LineKind::ToolResult(false), "boom");
    assert_eq!(fail[0].spans[0].content.as_ref(), "   ");
}

#[test]
fn streaming_does_not_yank_scrolled_view() {
    let mut t = Transcript::new();
    wide(&mut t);
    t.area.height = 3;
    fill(&mut t, 10);
    t.scroll_up(2);
    let anchored = t.top;
    assert!(anchored.is_some());
    t.push_stream(LineKind::Text, "more");
    assert_eq!(t.top, anchored, "reading position must not move");
}

#[test]
fn streaming_follows_when_pinned() {
    let mut t = Transcript::new();
    wide(&mut t);
    t.area.height = 3;
    fill(&mut t, 10);
    assert!(t.top.is_none());
    t.push_stream(LineKind::Text, "more");
    assert!(t.top.is_none());
    t.rewrap_stream();
    assert_eq!(
        t.current_top(),
        t.total_lines().saturating_sub(t.area.height as usize)
    );
}

#[test]
fn scroll_down_returns_to_bottom() {
    let mut t = Transcript::new();
    wide(&mut t);
    t.area.height = 3;
    fill(&mut t, 10);
    t.scroll_up(5);
    assert!(t.top.is_some());
    t.scroll_down(5);
    assert!(t.top.is_none());
}

#[test]
fn user_input_pins_to_bottom() {
    let mut t = Transcript::new();
    wide(&mut t);
    t.area.height = 3;
    fill(&mut t, 10);
    t.scroll_up(5);
    assert!(t.top.is_some());
    t.push_user("hello");
    assert!(t.top.is_none());
    assert_eq!(
        t.current_top(),
        t.total_lines().saturating_sub(t.area.height as usize)
    );
}

#[test]
fn shell_command_pins_to_bottom() {
    let mut t = Transcript::new();
    wide(&mut t);
    t.area.height = 3;
    fill(&mut t, 10);
    t.scroll_up(5);
    assert!(t.top.is_some());
    t.push_shell_command("ls");
    assert!(t.top.is_none());
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
    assert_eq!(row.text, "Ran 1 command, 1 failed");
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
fn unmatched_failed_tool_end_pushes_reason() {
    const REASON: &str = "tool 'file_write' is unavailable in Ask mode";
    let mut t = Transcript::new();
    t.on_event(&tool_end(1, false, REASON));
    assert_eq!(kinds_of(&t), vec![LineKind::System]);
    assert_eq!(t.rows[0].text, REASON);
}

#[test]
fn unmatched_rejected_tool_end_pushes_reason() {
    const REASON: &str = "tool execution was not performed: the user declined permission";
    let mut t = Transcript::new();
    t.on_event(&agent(AgentEvent::Tool(ToolEvent::Finished {
        call_id: ToolCallId(1),
        result: ToolResult::Rejected {
            reason: REASON.into(),
        },
    })));
    assert_eq!(kinds_of(&t), vec![LineKind::System]);
    assert_eq!(t.rows[0].text, REASON);
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
                raw_arguments: None,
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
    assert_eq!(t.rows[3].text, "Ran 1 command");
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

fn thinking_done(duration_ms: u64) -> AppEvent {
    agent(AgentEvent::Stream(StreamEvent::ThinkingDone {
        duration_ms,
    }))
}

fn stream_thinking(t: &mut Transcript, lines: usize) {
    for i in 1..=lines {
        let delta = format!("t{i:02}\n");
        t.on_event(&thinking(&delta));
    }
}

fn started() -> AppEvent {
    agent(AgentEvent::Turn(TurnEvent::Started))
}

fn completed() -> AppEvent {
    agent(AgentEvent::Turn(TurnEvent::Completed {
        usage: oven_llm::Usage::default(),
        duration_ms: 0,
    }))
}

fn cancelled() -> AppEvent {
    agent(AgentEvent::Turn(TurnEvent::Cancelled { duration_ms: 0 }))
}

fn loop_limit_reached(max_iters: usize) -> AppEvent {
    agent(AgentEvent::Turn(TurnEvent::LoopLimitReached {
        request_id: LoopLimitRequestId(1),
        max_iters,
    }))
}

fn kinds_of(t: &Transcript) -> Vec<LineKind> {
    t.rows.iter().map(|r| r.kind).collect()
}

fn all_details_collapsed(t: &Transcript) -> bool {
    t.rows
        .iter()
        .filter_map(|row| row.collapsible.as_ref())
        .all(|detail| !detail.is_expanded())
}

#[test]
fn tool_end_adds_no_extra_row() {
    let mut t = Transcript::new();
    t.push_user("q");
    t.on_event(&tool_start(
        1,
        "bash",
        serde_json::json!({ "command": "ls" }),
    ));
    t.on_event(&tool_end(1, true, "done"));
    assert_eq!(kinds_of(&t), vec![LineKind::User, LineKind::Tool,]);
    assert_eq!(t.rows[1].text, "Ran 1 command");
}

#[test]
fn loop_limit_reached_appends_system_line() {
    let mut t = Transcript::new();
    t.push_user("q");
    t.on_event(&loop_limit_reached(100));
    assert_eq!(kinds_of(&t), vec![LineKind::User, LineKind::System]);
    let expected = format!("{LOOP_LIMIT_REACHED} (100 iterations)");
    assert_eq!(t.rows.last().unwrap().text, expected);
}

#[test]
fn cancelled_appends_system_line() {
    let mut t = Transcript::new();
    t.push_user("q");
    t.on_event(&cancelled());
    assert_eq!(kinds_of(&t), vec![LineKind::User, LineKind::System]);
    assert_eq!(t.rows.last().map(|r| r.text.as_str()), Some("cancelled"));
}

#[test]
fn thinking_delta_shows_label_not_content() {
    let mut t = Transcript::new();
    t.on_event(&thinking("secret chain of thought"));
    t.on_event(&thinking(" more secrets"));
    t.on_event(&text_delta("answer"));
    t.on_event(&completed());
    assert_eq!(kinds_of(&t), vec![LineKind::Thinking, LineKind::Text]);
    assert_eq!(t.rows[0].text, THOUGHT_LABEL);
    assert_eq!(t.rows[1].text, "answer");
    assert!(t.rows.iter().all(|r| !r.text.contains("secret")));
}

#[test]
fn thinking_label_until_the_agent_reports_the_duration() {
    let mut t = Transcript::new();
    t.on_event(&thinking("planning"));
    assert_eq!(t.rows[0].text, THINKING_LABEL);

    t.on_event(&thinking_done(1_500));
    assert_eq!(t.rows[0].text, THOUGHT_1_5S);
    assert_eq!(t.rows[0].collapsible.as_ref().unwrap().body(), "planning");
}

#[test]
fn thinking_duration_lands_on_its_own_row() {
    const NOTICE: &str = "approval required";
    let mut t = Transcript::new();
    t.on_event(&thinking("planning"));
    t.push_row(LineKind::System, NOTICE);
    t.on_event(&thinking_done(1_500));

    assert_eq!(t.rows[0].text, THOUGHT_1_5S);
    assert_eq!(t.rows[1].text, NOTICE);
}

#[test]
fn reported_thinking_duration_survives_the_answer() {
    let mut t = Transcript::new();
    t.push_user("q");
    t.on_event(&thinking("planning"));
    t.on_event(&thinking_done(1_500));
    t.on_event(&text_delta("answer"));
    t.on_event(&completed());

    assert_eq!(
        kinds_of(&t),
        vec![LineKind::User, LineKind::Thinking, LineKind::Text]
    );
    assert_eq!(t.rows[1].text, THOUGHT_1_5S);
}

#[test]
fn reported_thinking_duration_survives_the_tool_it_led_to() {
    let mut t = Transcript::new();
    t.on_event(&thinking("planning"));
    t.on_event(&thinking_done(1_500));
    t.on_event(&tool_start(
        1,
        "bash",
        serde_json::json!({ "command": "ls" }),
    ));

    assert_eq!(t.rows[0].text, THOUGHT_1_5S);
    assert_eq!(t.rows[1].kind, LineKind::Tool);
}

#[test]
fn seeded_reasoning_renders_before_the_answer_it_precedes() {
    let mut t = Transcript::new();
    t.seed_timed(&[
        (Message::user_text("q"), 0, None),
        (
            Message::assistant(vec![
                ContentBlock::Text {
                    text: "answer".into(),
                },
                ContentBlock::Thinking {
                    thinking: "reasoning".into(),
                },
            ]),
            1_000,
            Some(1_500),
        ),
    ]);

    assert_eq!(
        kinds_of(&t),
        vec![LineKind::User, LineKind::Thinking, LineKind::Text]
    );
    assert_eq!(t.rows[1].text, THOUGHT_1_5S);
    assert_eq!(t.rows[2].text, "answer");
}

#[test]
fn tool_result_double_click_toggles_detail() {
    const OUTPUT: &str = "updated\nmore lines of output";

    let mut t = Transcript::new();
    t.on_event(&tool_start(10, "todo_write", todo_input()));
    t.on_event(&tool_end(10, true, OUTPUT));
    ready(&mut t, Rect::new(0, 0, 80, 10));

    let detail = t.rows[1].collapsible.as_ref().expect("tool result detail");
    assert_eq!(detail.body(), OUTPUT);
    assert!(detail.is_expanded());
    assert!(
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains("more lines of output"))
    );

    let header_y = t
        .wrapped
        .iter()
        .position(|line| line_text(line).contains(RESULT_LABEL))
        .expect("result header") as u16;
    double_click(&mut t, 2, header_y);

    assert!(
        !t.rows[1]
            .collapsible
            .as_ref()
            .expect("tool result detail")
            .is_expanded()
    );
    assert!(
        t.wrapped
            .iter()
            .all(|line| !line_text(line).contains("more lines of output"))
    );

    t.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), 2, header_y),
        &State::new(),
    );
    double_click(&mut t, 2, header_y);
    assert!(
        t.rows[1]
            .collapsible
            .as_ref()
            .expect("tool result detail")
            .is_expanded()
    );
}

#[test]
fn thinking_double_click_toggles_detail() {
    const THINKING: &str = "inspect the implementation";

    let mut t = Transcript::new();
    t.on_event(&thinking(THINKING));
    t.on_event(&completed());
    ready(&mut t, Rect::new(0, 0, 80, 8));

    let detail = t.rows[0].collapsible.as_ref().expect("thinking detail");
    assert_eq!(detail.body(), THINKING);
    assert!(!detail.is_expanded());
    assert!(t.wrapped.iter().all(|line| {
        !line
            .spans
            .iter()
            .any(|span| span.content.contains(THINKING))
    }));

    double_click(&mut t, 2, 0);

    assert!(
        t.rows[0]
            .collapsible
            .as_ref()
            .expect("thinking detail")
            .is_expanded()
    );
    assert!(t.wrapped.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.contains(THINKING))
    }));

    t.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), 2, 0),
        &State::new(),
    );
    double_click(&mut t, 2, 0);
    assert!(
        !t.rows[0]
            .collapsible
            .as_ref()
            .expect("thinking detail")
            .is_expanded()
    );
}

#[test]
fn expanding_a_block_grows_downward_and_keeps_its_header() {
    const DETAIL: &str = "body one\nbody two\nbody three";

    let mut t = Transcript::new();
    for i in 0..8 {
        t.push_row(LineKind::Text, &format!("line {i}"));
    }
    t.push_row(LineKind::ToolResult(true), DETAIL);
    for i in 0..2 {
        t.push_row(LineKind::Text, &format!("tail {i}"));
    }
    ready(&mut t, Rect::new(0, 0, 80, 6));
    let detail = t.rows[8].collapsible.as_ref().expect("result detail");
    assert!(!detail.is_expanded(), "a later row collapsed it");

    let header = t.rows[8].headers[0].line;
    let top = t.current_top();
    assert!(t.top.is_none(), "the view follows the bottom");
    assert!(top <= header && header < top + 6, "header is on screen");

    double_click(&mut t, 2, u16::try_from(header - top).expect("screen row"));

    assert!(
        t.rows[8]
            .collapsible
            .as_ref()
            .expect("result detail")
            .is_expanded()
    );
    assert_eq!(
        t.current_top(),
        top,
        "the header must stay where it was clicked"
    );
    assert!(t.top.is_some(), "the anchor pins the view");
    let start = t.current_top();
    assert!(start <= header && header < start + 6);
    assert!(
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains("body two"))
    );
}

#[test]
fn next_message_collapses_previous_details() {
    let mut t = Transcript::new();
    t.on_event(&thinking("plan"));
    assert!(t.rows[0].collapsible.as_ref().unwrap().is_expanded());

    t.on_event(&tool_start(1, "file_edit", file_edit_input()));
    t.on_event(&tool_end(1, true, "edited"));
    assert!(!t.rows[0].collapsible.as_ref().unwrap().is_expanded());
    assert!(t.rows[1].collapsible.as_ref().unwrap().is_expanded());

    t.on_event(&text_delta("done"));
    assert!(!t.rows[1].collapsible.as_ref().unwrap().is_expanded());
}

#[test]
fn expanding_during_generation_stays_open() {
    let mut t = Transcript::new();
    t.on_event(&thinking("plan"));
    t.on_event(&text_delta("ans"));
    assert!(!t.rows[0].collapsible.as_ref().unwrap().is_expanded());

    t.rows[0]
        .collapsible
        .as_mut()
        .expect("thinking detail")
        .toggle();
    t.on_event(&text_delta("wer"));
    t.on_event(&completed());
    assert!(
        t.rows[0].collapsible.as_ref().unwrap().is_expanded(),
        "manual expand must survive later tokens in the same message"
    );
}

#[test]
fn manual_expand_survives_next_thinking() {
    let mut t = Transcript::new();
    t.on_event(&thinking("one"));
    t.on_event(&tool_start(1, "file_edit", file_edit_input()));
    t.on_event(&tool_end(1, true, "edited"));
    assert!(!t.rows[0].collapsible.as_ref().unwrap().is_expanded());

    t.rows[0]
        .collapsible
        .as_mut()
        .expect("thinking detail")
        .toggle();
    t.on_event(&thinking("two"));

    assert!(t.rows[0].collapsible.as_ref().unwrap().is_expanded());
    assert!(
        t.rows
            .last()
            .unwrap()
            .collapsible
            .as_ref()
            .unwrap()
            .is_expanded()
    );
}

#[test]
fn ended_stream_collapses_its_details() {
    let mut t = Transcript::new();
    t.on_event(&thinking("plan"));
    t.on_event(&thinking_done(1_500));
    assert!(
        !t.rows[0].collapsible.as_ref().unwrap().is_expanded(),
        "streaming ended, so the windowed body is gone"
    );

    t.on_event(&started());
    t.push_user("next");
    assert!(!t.rows[0].collapsible.as_ref().unwrap().is_expanded());
}

#[test]
fn seed_collapsibles_stay_collapsed() {
    let mut t = Transcript::new();
    t.seed(&[
        Message::assistant(vec![
            ContentBlock::Thinking {
                thinking: "one".into(),
            },
            ContentBlock::ToolUse {
                id: "e1".into(),
                name: "file_edit".into(),
                input: file_edit_input(),
                raw_arguments: None,
            },
        ]),
        Message::tool_result(
            "e1",
            "edited src/main.rs: replaced 1 occurrence(s), 3 bytes -> 3 bytes",
            false,
        ),
    ]);
    assert!(all_details_collapsed(&t));
}

#[test]
fn thinking_hover_paints_gray_background() {
    let hover_bg = theme::hover().bg.expect("hover bg");

    let mut t = Transcript::new();
    t.push_user("q");
    t.on_event(&thinking("secret"));
    t.on_event(&completed());
    let area = Rect::new(0, 0, 40, 6);
    ready(&mut t, area);

    let header_y = 2;
    let backend = TestBackend::new(area.width, area.height);
    let mut terminal = Terminal::new(backend).unwrap();

    t.handle_mouse(mouse(MouseEventKind::Moved, 3, header_y), &State::new());
    terminal
        .draw(|f| t.draw(f, f.area(), &State::new()))
        .unwrap();
    let buf = terminal.backend().buffer();
    assert_eq!(buf[(3, header_y)].style().bg, Some(hover_bg));
    assert_eq!(
        buf[(area.width - 1, header_y)].style().bg,
        Some(hover_bg),
        "hover must cover trailing blank cells"
    );
    assert_ne!(buf[(3, 0)].style().bg, Some(hover_bg));

    t.handle_mouse(mouse(MouseEventKind::Moved, 3, 0), &State::new());
    terminal
        .draw(|f| t.draw(f, f.area(), &State::new()))
        .unwrap();
    let buf = terminal.backend().buffer();
    assert_ne!(buf[(3, header_y)].style().bg, Some(hover_bg));
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
    assert_eq!(kinds_of(&t), vec![LineKind::Thinking, LineKind::Text]);
    assert_eq!(t.rows[0].text, THOUGHT_LABEL);
    assert_eq!(
        t.rows[0]
            .collapsible
            .as_ref()
            .expect("thinking detail")
            .body(),
        "onetwo"
    );
}

#[test]
fn seed_without_thinking_record_keeps_thought_label() {
    let mut t = Transcript::new();
    t.seed_timed(&[(
        Message::assistant(vec![ContentBlock::Thinking {
            thinking: "plan".into(),
        }]),
        1_000,
        None,
    )]);
    assert_eq!(t.rows[0].text, THOUGHT_LABEL);
}

#[test]
fn seed_timed_shows_thought_duration() {
    let mut t = Transcript::new();
    t.seed_timed(&[(
        Message::assistant(vec![
            ContentBlock::Thinking {
                thinking: "plan".into(),
            },
            ContentBlock::Text { text: "ok".into() },
        ]),
        2_500,
        Some(1_500),
    )]);
    assert_eq!(t.rows[0].kind, LineKind::Thinking);
    assert_eq!(t.rows[0].text, THOUGHT_1_5S);
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
    assert_eq!(t.rows[0].text, "Ran 2 commands, Read 1 file, 1 failed");
    assert_eq!(
        t.rows[0].collapsible.as_ref().expect("burst detail").body(),
        "Ran ls\nRan pwd\nRead a"
    );
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
    assert_eq!(t.rows[0].text, "Ran 1 command");
    t.on_event(&tool_end(1, false, "boom"));
    assert_eq!(t.rows.len(), 1);
    assert_eq!(t.rows[0].kind, LineKind::Tool);
    assert_eq!(t.rows[0].text, "Ran 1 command, 1 failed");
}

#[test]
fn burst_keeps_its_own_row_when_another_row_interleaves() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(
        1,
        "bash",
        serde_json::json!({ "command": "ls" }),
    ));
    t.on_event(&tool_end(1, true, "ok"));
    t.on_event(&agent(AgentEvent::Tool(ToolEvent::ApprovalRequested {
        request_id: ApprovalRequestId(1),
        call_id: ToolCallId(2),
        name: "bash".into(),
        view: present_tool("bash", &serde_json::json!({ "command": "rm -rf /" })),
    })));
    t.on_event(&tool_start(
        2,
        "bash",
        serde_json::json!({ "command": "pwd" }),
    ));
    t.on_event(&tool_end(2, true, "ok"));

    assert_eq!(kinds_of(&t), vec![LineKind::Tool, LineKind::System]);
    assert_eq!(t.rows[0].text, "Ran 2 commands");
    assert_eq!(
        t.rows[0].collapsible.as_ref().expect("burst detail").body(),
        "Ran ls\nRan pwd"
    );
    assert_eq!(t.rows[1].text, "approval required: Ran rm -rf /");
}

#[test]
fn burst_collapses_once_the_next_row_arrives() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(
        1,
        "grep",
        serde_json::json!({ "pattern": "todo", "path": "src" }),
    ));
    t.on_event(&tool_end(1, true, "src/main.rs:1:todo"));
    assert!(
        t.rows[0]
            .collapsible
            .as_ref()
            .expect("burst detail")
            .is_expanded()
    );

    t.on_event(&text_delta("done"));
    assert!(
        !t.rows[0]
            .collapsible
            .as_ref()
            .expect("burst detail")
            .is_expanded()
    );
}

#[test]
fn burst_double_click_toggles_call_list() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(
        1,
        "file_read",
        serde_json::json!({ "path": "src/main.rs" }),
    ));
    t.on_event(&tool_end(1, true, "hi"));
    t.on_event(&text_delta("done"));
    ready(&mut t, Rect::new(0, 0, 80, 10));

    assert!(
        t.wrapped
            .iter()
            .all(|line| !line_text(line).contains("Read src/main.rs"))
    );
    let header_y = t
        .wrapped
        .iter()
        .position(|line| line_text(line).contains("Read 1 file"))
        .expect("burst header") as u16;
    double_click(&mut t, 2, header_y);

    assert!(
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains("Read src/main.rs"))
    );
}

const EARLIER_4: &str = "… 4 earlier lines";
const EARLIER_2_CALLS: &str = "… 2 earlier calls";

#[test]
fn live_thinking_body_windows_to_the_newest_lines() {
    let mut t = Transcript::new();
    stream_thinking(&mut t, MAX_LIVE_BODY_LINES + 4);
    ready(&mut t, Rect::new(0, 0, 80, 24));
    let has = |needle: &str| {
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains(needle))
    };

    assert!(has(EARLIER_4), "{:?}", t.wrapped);
    assert!(has("t12"));
    assert!(!has("t04"));

    t.on_event(&thinking_done(1_500));
    let has = |needle: &str| {
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains(needle))
    };
    assert!(!has(EARLIER_4));
    assert!(!has("t12"), "no gap is left before the next row");

    double_click(&mut t, 2, 0);
    let has = |needle: &str| {
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains(needle))
    };
    assert!(!has(EARLIER_4));
    assert!(has("t01"));
    assert!(has("t12"));
}

#[test]
fn open_tool_burst_body_windows_to_the_newest_calls() {
    let mut t = Transcript::new();
    for i in 1..=(MAX_LIVE_BODY_LINES + 2) as u64 {
        t.on_event(&tool_start(
            i,
            "bash",
            serde_json::json!({ "command": format!("c{i:02}") }),
        ));
    }
    ready(&mut t, Rect::new(0, 0, 80, 24));
    let has = |needle: &str| {
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains(needle))
    };

    assert!(has(EARLIER_2_CALLS), "{:?}", t.wrapped);
    assert!(has("c10"));
    assert!(!has("c01"));

    t.on_event(&completed());
    let has = |needle: &str| {
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains(needle))
    };
    assert!(!has(EARLIER_2_CALLS));
    assert!(!has("c10"), "the burst closes once the turn moves on");

    let header = t
        .wrapped
        .iter()
        .position(|line| line_text(line).contains("Ran 10 commands"))
        .expect("burst header") as u16;
    double_click(&mut t, 2, header);
    let has = |needle: &str| {
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains(needle))
    };
    assert!(!has(EARLIER_2_CALLS));
    assert!(has("c01"));
    assert!(has("c10"));
}

#[test]
fn live_diff_burst_windows_to_the_newest_calls() {
    let mut t = Transcript::new();
    for i in 1..=(MAX_LIVE_BODY_LINES + 2) {
        t.on_event(&tool_start(
            i as u64,
            "file_edit",
            serde_json::json!({
                "path": format!("src/f{i:02}.rs"),
                "old_string": "old",
                "new_string": "new"
            }),
        ));
    }
    ready(&mut t, Rect::new(0, 0, 80, 40));
    let has = |needle: &str| {
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains(needle))
    };

    assert!(has(EARLIER_2_CALLS), "{:?}", t.wrapped);
    assert!(has("Edit src/f10.rs"));
    assert!(!has("Edit src/f01.rs"));
    assert!(has("- old"), "a visible item still shows its diff");

    t.on_event(&completed());
    let has = |needle: &str| {
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains(needle))
    };
    assert!(!has(EARLIER_2_CALLS));
    assert!(
        !has("Edit src/f10.rs"),
        "the burst closes once the turn moves on"
    );

    let row = wrapped_row_of(&t, "Edited 10 files");
    double_click(&mut t, 2, row);
    let has = |needle: &str| {
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains(needle))
    };
    assert!(has("Edit src/f01.rs") && has("Edit src/f10.rs"));
}

fn todo_input() -> serde_json::Value {
    serde_json::json!({
        "todos": [{"id": "a", "content": "one", "status": "pending"}]
    })
}

fn file_edit_input() -> serde_json::Value {
    serde_json::json!({
        "path": "src/main.rs",
        "old_string": "old",
        "new_string": "new"
    })
}

fn file_edit_input_at(path: &str) -> serde_json::Value {
    serde_json::json!({
        "path": path,
        "old_string": "libold",
        "new_string": "libnew"
    })
}

fn file_write_input() -> serde_json::Value {
    serde_json::json!({
        "path": "out.txt",
        "content": "line one\nline two"
    })
}

/// Diff calls of a burst row as `(item title, item body)` pairs.
fn diff_items(t: &Transcript) -> Vec<(String, String)> {
    t.rows[0]
        .collapsible
        .as_ref()
        .expect("burst detail")
        .visible_sections(None)
        .1
        .iter()
        .filter_map(|section| match section {
            Section::Item { title, detail, .. } => Some((title.clone(), detail.body())),
            Section::Text(_) => None,
        })
        .collect()
}

fn wrapped_row_of(t: &Transcript, needle: &str) -> u16 {
    t.wrapped
        .iter()
        .position(|line| line_text(line).contains(needle))
        .map(|idx| u16::try_from(idx).expect("screen row"))
        .expect("wrapped line")
}

const EDIT_SUCCESS: &str = "edited src/main.rs: replaced 1 occurrence(s), 3 bytes -> 3 bytes";
const EDIT_ERROR: &str = "old_string not found";

#[test]
fn file_edit_aggregates_with_a_nested_diff() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(1, "file_edit", file_edit_input()));
    t.on_event(&tool_end(1, true, EDIT_SUCCESS));

    assert_eq!(kinds_of(&t), vec![LineKind::Tool]);
    assert_eq!(t.rows[0].text, "Edited 1 file");
    let burst = t.rows[0].collapsible.as_ref().expect("burst detail");
    assert!(burst.is_expanded());
    assert_eq!(burst.body(), "");
    assert_eq!(
        diff_items(&t),
        [("Edit src/main.rs".to_string(), "- old\n+ new".to_string())]
    );
    assert!(
        t.wrapped
            .iter()
            .all(|line| !line_text(line).contains("replaced 1 occurrence"))
    );
}

#[test]
fn file_write_aggregates_with_a_nested_diff() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(2, "file_write", file_write_input()));
    t.on_event(&tool_end(2, true, "wrote 17 bytes to out.txt"));

    assert_eq!(kinds_of(&t), vec![LineKind::Tool]);
    assert_eq!(t.rows[0].text, "Wrote 1 file");
    assert_eq!(
        diff_items(&t),
        [(
            "Write out.txt".to_string(),
            "+ line one\n+ line two".to_string()
        )]
    );
}

#[test]
fn mixed_burst_keeps_calls_in_invocation_order() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(1, "file_edit", file_edit_input()));
    t.on_event(&tool_start(
        2,
        "bash",
        serde_json::json!({ "command": "cargo test" }),
    ));
    t.on_event(&tool_end(1, true, EDIT_SUCCESS));
    t.on_event(&tool_end(2, true, "ok"));

    assert_eq!(t.rows[0].text, "Edited 1 file, Ran 1 command");
    assert_eq!(
        diff_items(&t),
        [("Edit src/main.rs".to_string(), "- old\n+ new".to_string())]
    );
    assert_eq!(
        t.rows[0].collapsible.as_ref().expect("burst detail").body(),
        "Ran cargo test"
    );
}

#[test]
fn failed_edit_counts_and_explains_itself() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(1, "file_edit", file_edit_input()));
    t.on_event(&tool_end(1, false, EDIT_ERROR));

    assert_eq!(kinds_of(&t), vec![LineKind::Tool]);
    assert_eq!(t.rows[0].text, "Edited 1 file, 1 failed");
    assert_eq!(
        diff_items(&t),
        [(
            "Edit src/main.rs".to_string(),
            format!("- old\n+ new\n{EDIT_ERROR}")
        )]
    );
}

#[test]
fn seed_file_edit_aggregates_with_a_nested_diff() {
    let mut t = Transcript::new();
    t.seed(&[
        Message::assistant(vec![ContentBlock::ToolUse {
            id: "e1".into(),
            name: "file_edit".into(),
            input: file_edit_input(),
            raw_arguments: None,
        }]),
        Message::tool_result("e1", EDIT_SUCCESS, false),
    ]);
    assert_eq!(kinds_of(&t), vec![LineKind::Tool]);
    assert_eq!(t.rows[0].text, "Edited 1 file");
    assert_eq!(
        diff_items(&t),
        [("Edit src/main.rs".to_string(), "- old\n+ new".to_string())]
    );
    assert!(!t.rows[0].collapsible.as_ref().unwrap().is_expanded());
}

#[test]
fn diff_double_click_toggles_burst_detail() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(1, "file_edit", file_edit_input()));
    t.on_event(&tool_end(1, true, EDIT_SUCCESS));
    t.on_event(&text_delta("done"));
    ready(&mut t, Rect::new(0, 0, 80, 10));
    let has = |t: &Transcript| {
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains("- old"))
    };
    assert!(!has(&t), "the burst collapses once the turn moves on");

    let row = wrapped_row_of(&t, "Edited 1 file");
    double_click(&mut t, 2, row);
    assert!(
        t.rows[0]
            .collapsible
            .as_ref()
            .expect("burst detail")
            .is_expanded()
    );
    assert!(
        !has(&t),
        "a reopened burst lists its diffs as titles, not their contents"
    );
    assert!(
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains("Edit src/main.rs"))
    );

    let row = wrapped_row_of(&t, "Edit src/main.rs");
    double_click(&mut t, 2, row);
    assert!(has(&t));

    let row = wrapped_row_of(&t, "Edit src/main.rs");
    double_click(&mut t, 2, row);
    assert!(!has(&t));
    assert!(
        t.rows[0]
            .collapsible
            .as_ref()
            .expect("burst detail")
            .is_expanded(),
        "the burst itself stays expanded"
    );
}

#[test]
fn nested_item_double_click_toggles_only_its_own_diff() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(1, "file_edit", file_edit_input()));
    t.on_event(&tool_start(
        2,
        "file_edit",
        file_edit_input_at("src/lib.rs"),
    ));
    t.on_event(&tool_end(1, true, EDIT_SUCCESS));
    t.on_event(&tool_end(2, true, EDIT_SUCCESS));
    t.on_event(&text_delta("done"));
    ready(&mut t, Rect::new(0, 0, 80, 12));
    let has = |needle: &str, t: &Transcript| {
        t.wrapped
            .iter()
            .any(|line| line_text(line).contains(needle))
    };
    assert!(!has("- old", &t) && !has("- libold", &t));

    let row = wrapped_row_of(&t, "Edited 2 files");
    double_click(&mut t, 2, row);
    assert!(
        !has("- old", &t) && !has("- libold", &t),
        "items stay folded"
    );
    assert!(has("Edit src/main.rs", &t) && has("Edit src/lib.rs", &t));

    let row = wrapped_row_of(&t, "Edit src/main.rs");
    double_click(&mut t, 2, row);
    assert!(has("- old", &t), "the clicked item expands");
    assert!(!has("- libold", &t), "its sibling keeps its own state");
    assert!(
        t.rows[0]
            .collapsible
            .as_ref()
            .expect("burst detail")
            .is_expanded(),
        "the burst itself stays expanded"
    );

    let row = wrapped_row_of(&t, "Edit src/lib.rs");
    double_click(&mut t, 2, row);
    assert!(has("- old", &t) && has("- libold", &t));

    let row = wrapped_row_of(&t, "Edited 2 files");
    double_click(&mut t, 2, row);
    assert!(!has("- old", &t) && !has("- libold", &t) && !has("Edit src/main.rs", &t));
}

#[test]
fn nested_diff_lines_are_indented_past_their_item_title() {
    let mut t = Transcript::new();
    t.on_event(&tool_start(1, "file_edit", file_edit_input()));
    t.on_event(&tool_end(1, true, EDIT_SUCCESS));
    ready(&mut t, Rect::new(0, 0, 80, 10));

    let removed = t
        .wrapped
        .iter()
        .find(|line| line_text(line).contains("- old"))
        .expect("removed line");
    let added = t
        .wrapped
        .iter()
        .find(|line| line_text(line).contains("+ new"))
        .expect("added line");
    let item_indent = format!("{MESSAGE_INDENT}{}{LINE_INDENT}", LINE_INDENT);
    assert_eq!(removed.spans[0].content.as_ref(), item_indent);
    assert_eq!(added.spans[0].content.as_ref(), item_indent);
    assert_eq!(removed.spans[1].content.as_ref(), "- old");
    assert_eq!(added.spans[1].content.as_ref(), "+ new");
    assert_eq!(
        removed.spans[1].style.bg,
        Some(ratatui::style::Color::LightRed)
    );
    assert_eq!(
        added.spans[1].style.bg,
        Some(ratatui::style::Color::LightGreen)
    );
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
    assert_eq!(t.rows[1].text, RESULT_LABEL);
    let result = t.rows[1].collapsible.as_ref().expect("tool result detail");
    assert_eq!(result.body(), "updated");
    assert!(result.is_expanded());
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
    assert_eq!(t.rows[0].text, "Ran 1 command");
    assert_eq!(
        t.rows[1].text,
        "todo_write · 1 todos (0 in_progress, 0 completed)"
    );
    assert_eq!(t.rows[3].text, "Ran 1 command");
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
                raw_arguments: None,
            },
            ContentBlock::ToolUse {
                id: "c2".into(),
                name: "glob".into(),
                input: glob,
                raw_arguments: None,
            },
        ]),
        Message::tool_result("c1", "event.rs:1:ToolEvent", false),
        Message::tool_result("c2", "crates/oven-agent/src/agent.rs", false),
    ]);

    assert_eq!(kinds_of(&live), kinds_of(&restored));
    let live_rows: Vec<_> = live.rows.iter().map(|row| row.text.as_str()).collect();
    let restored_rows: Vec<_> = restored.rows.iter().map(|row| row.text.as_str()).collect();
    assert_eq!(live_rows, restored_rows);
    assert_eq!(live_rows, vec!["Searched 2 patterns"]);
    assert_eq!(
        live.rows[0]
            .collapsible
            .as_ref()
            .expect("burst detail")
            .body(),
        restored.rows[0]
            .collapsible
            .as_ref()
            .expect("burst detail")
            .body()
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
            raw_arguments: None,
        }]),
        Message::tool_result("t1", "updated", false),
    ]);
    assert_eq!(
        kinds_of(&t),
        vec![LineKind::Tool, LineKind::ToolResult(true)]
    );
    assert_eq!(
        t.rows[0].text,
        "todo_write · 1 todos (0 in_progress, 0 completed)"
    );
    assert_eq!(t.rows[1].text, RESULT_LABEL);
    assert_eq!(
        t.rows[1]
            .collapsible
            .as_ref()
            .expect("tool result detail")
            .body(),
        "updated"
    );
    assert!(!t.rows[1].collapsible.as_ref().unwrap().is_expanded());
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
                raw_arguments: None,
            },
            ContentBlock::ToolUse {
                id: "c2".into(),
                name: "bash".into(),
                input: serde_json::json!({ "command": "pwd" }),
                raw_arguments: None,
            },
            ContentBlock::ToolUse {
                id: "c3".into(),
                name: "file_read".into(),
                input: serde_json::json!({ "path": "a" }),
                raw_arguments: None,
            },
        ]),
        Message::tool_result("c1", "ok", false),
        Message::tool_result("c2", "boom", true),
        Message::tool_result("c3", "hi", false),
    ]);
    assert_eq!(kinds_of(&t), vec![LineKind::Tool]);
    assert_eq!(t.rows[0].text, "Ran 2 commands, Read 1 file, 1 failed");
}

#[test]
fn format_thought_units() {
    assert_eq!(format_thought(None), THOUGHT_LABEL);
    assert_eq!(format_thought(Some(0)), THOUGHT_0);
    assert_eq!(format_thought(Some(99)), THOUGHT_0);
    assert_eq!(format_thought(Some(1_500)), THOUGHT_1_5S);
    assert_eq!(format_thought(Some(61_000)), THOUGHT_1M_1S);
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
    t.area.height = 5;
    fill(&mut t, 20);
    let bottom = t.total_lines().saturating_sub(5);
    let page = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
    t.handle_key(page, &State::new());
    assert_eq!(t.top, Some(bottom.saturating_sub(5)));
    t.handle_key(
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
        &State::new(),
    );
    assert!(t.top.is_none());
}

fn ready(t: &mut Transcript, area: Rect) {
    t.area = area;
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

fn line_text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn double_click(t: &mut Transcript, column: u16, row: u16) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
        MouseEventKind::Down(MouseButton::Left),
    ] {
        assert!(matches!(
            t.handle_mouse(mouse(kind, column, row), &State::new()),
            KeyResult::Handled
        ));
    }
}

#[test]
fn slice_cols_by_display_width() {
    assert_eq!(slice_cols("hello", 1, 4), "ell");
    assert_eq!(slice_cols("hello", 3, 3), "");
    assert_eq!(slice_cols("你好", 0, 2), "你");
    assert_eq!(slice_cols("你好", 2, 4), "好");
    assert_eq!(slice_cols("你好", 1, 2), "你");
    assert_eq!(slice_cols("你好", 1, 3), "你好");
    assert_eq!(slice_cols("한글", 1, 2), "한");
    assert_eq!(slice_cols("한글", 1, 3), "한글");
    assert_eq!(slice_cols("こんにちは", 1, 3), "こん");
}

#[test]
fn extract_skips_gutter() {
    let line = format_lines(LineKind::Text, "hello").pop().unwrap();
    assert_eq!(extract_line_range(&line, 0, 8), "hello");
    assert_eq!(extract_line_range(&line, 3, 8), "hello");
    assert_eq!(extract_line_range(&line, 3, 6), "hel");
    assert_eq!(extract_line_range(&line, 0, 3), "");
}

#[test]
fn mouse_drag_selects_body_without_gutter() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "hello");
    ready(&mut t, Rect::new(0, 0, 80, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 3, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 8, 0),
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
        mouse(MouseEventKind::Down(MouseButton::Left), 8, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 3, 0),
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
        mouse(MouseEventKind::Down(MouseButton::Left), 3, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 8, 2),
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
fn drag_survives_events_that_rewrap_the_transcript() {
    let mut t = Transcript::new();
    t.on_event(&text_delta("first answer"));
    t.on_event(&completed());
    t.on_event(&tool_start(1, "todo_write", todo_input()));
    t.on_event(&tool_end(1, true, "ok"));
    ready(&mut t, Rect::new(0, 0, 80, 10));

    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 3, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 12, 0),
        &State::new(),
    );
    let selected = t.selected_text();
    assert!(selected.is_some());

    t.on_event(&tool_start(2, "todo_write", todo_input()));
    t.on_event(&tool_end(2, true, "ok"));
    assert!(t.dragging);
    assert_eq!(t.selected_text(), selected);

    let up = t.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), 12, 0),
        &State::new(),
    );
    assert!(matches!(up, KeyResult::Action(Action::Notify(text)) if text == "Copied!"));
}

#[test]
fn up_copies_when_drag_state_was_lost() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "hello");
    ready(&mut t, Rect::new(0, 0, 80, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 3, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 8, 0),
        &State::new(),
    );
    t.dragging = false;
    let up = t.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), 8, 0),
        &State::new(),
    );
    assert!(matches!(up, KeyResult::Action(Action::Notify(text)) if text == "Copied!"));
}

#[test]
fn mouse_up_after_selection_emits_copied_reply() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "hello");
    ready(&mut t, Rect::new(0, 0, 80, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 3, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 8, 0),
        &State::new(),
    );
    let up = t.handle_mouse(
        mouse(MouseEventKind::Up(MouseButton::Left), 8, 0),
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
fn mouse_selects_wide_chars_from_trailing_cell() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "你好");
    ready(&mut t, Rect::new(0, 0, 80, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 3, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 4, 0),
        &State::new(),
    );
    assert_eq!(t.selected_text().as_deref(), Some("你"));
}

#[test]
fn selecting_cjk_does_not_expand_drawn_line() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "你好世界");
    ready(&mut t, Rect::new(0, 0, 20, 3));
    let mut terminal = Terminal::new(TestBackend::new(20, 3)).unwrap();
    terminal
        .draw(|f| t.draw(f, f.area(), &State::new()))
        .unwrap();
    let glyphs = |row: String| {
        row.chars()
            .filter(|c| !c.is_whitespace() && *c != '∙')
            .collect::<String>()
    };
    let before = {
        let buf = terminal.backend().buffer();
        glyphs((0..20).map(|x| buf[(x, 0)].symbol().to_string()).collect())
    };
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 3, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 8, 0),
        &State::new(),
    );
    terminal
        .draw(|f| t.draw(f, f.area(), &State::new()))
        .unwrap();
    let after = {
        let buf = terminal.backend().buffer();
        glyphs((0..20).map(|x| buf[(x, 0)].symbol().to_string()).collect())
    };
    assert_eq!(after, before, "selection must not duplicate CJK glyphs");
    assert_eq!(after, "你好世界");
}

#[test]
fn mouse_selects_wrapped_lines() {
    let mut t = Transcript::new();
    t.push_row(LineKind::Text, "abcdefgh");
    ready(&mut t, Rect::new(0, 0, 7, 5));
    t.handle_mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 3, 0),
        &State::new(),
    );
    t.handle_mouse(
        mouse(MouseEventKind::Drag(MouseButton::Left), 7, 1),
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
    let hi = highlight_line(&line, 3, 8);
    assert_eq!(hi.spans.len(), 2);
    assert_eq!(hi.spans[0].content.as_ref(), " ∙ ");
    assert_eq!(hi.spans[1].content.as_ref(), "hello");
    assert_eq!(hi.spans[1].style, theme::selection());
}

#[test]
fn highlight_wide_chars_are_not_duplicated() {
    let line = format_lines(LineKind::Text, "한글中文").pop().unwrap();
    let original = line_text(&line);
    let width = line_display_width(&line);
    for from in 0..width {
        for to in from + 1..=width {
            let hi = highlight_line(&line, from, to);
            assert_eq!(line_text(&hi), original, "from={from} to={to}");
            assert_eq!(line_display_width(&hi), width, "from={from} to={to}");
        }
    }
}

#[test]
fn highlight_mid_cjk_cell_keeps_whole_glyph() {
    let line = format_lines(LineKind::Text, "你好").pop().unwrap();
    let hi = highlight_line(&line, 3, 4);
    assert_eq!(line_text(&hi), line_text(&line));
    assert_eq!(hi.spans[1].content.as_ref(), "你");
    assert_eq!(hi.spans[1].style, theme::selection());
    assert_eq!(hi.spans[2].content.as_ref(), "好");
    assert_eq!(hi.spans[2].style, ratatui::style::Style::default());
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
    assert!(row.starts_with(" ∙ 好"), "{row:?}");
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

use super::draw::composer_hint;
use super::*;
use crate::core::hint::{self, Keys, Strip};
use crate::widgets::agents::{main_line, running};
use crate::widgets::input::{InputView, Overlay};
use crate::widgets::question_prompt::{QuestionPrompt, QuestionPromptAction};
use crate::widgets::slash_command_popup::SlashCommandPopup;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use oven_app::config::ProviderConfig;
use oven_app::{
    AgentEvent, AppEventKind, NodeStatus, ToolCallId, ToolEvent, ToolResult, TurnEvent, TurnId,
};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

#[test]
fn the_unsent_notice_counts_messages() {
    assert_eq!(unsent_notice(1), "dropped 1 queued message (never sent)");
    assert_eq!(unsent_notice(3), "dropped 3 queued messages (never sent)");
}

#[test]
fn send_each_sends_messages_separately_in_order() {
    let mut sent = Vec::new();
    let remaining = send_each(
        vec!["one".to_string(), "two".to_string(), "three".to_string()],
        |text| {
            sent.push(text.to_string());
            true
        },
    );
    assert!(remaining.is_empty());
    assert_eq!(sent, vec!["one", "two", "three"]);
}

#[test]
fn send_each_stops_at_first_failure_and_returns_remainder() {
    let mut calls = Vec::new();
    let remaining = send_each(
        vec!["one".to_string(), "two".to_string(), "three".to_string()],
        |text| {
            calls.push(text.to_string());
            text != "two"
        },
    );
    assert_eq!(calls, vec!["one", "two"]);
    assert_eq!(remaining, vec!["two", "three"]);
}

#[tokio::test]
async fn the_open_popup_states_its_keys_on_the_composer_border() {
    let root = tempdir::TempDir::new("oven-ui-hint").unwrap();
    let mut ui = test_ui(&root).await;
    ui.input.set_text("/");

    let mut terminal = Terminal::new(TestBackend::new(60, 6)).unwrap();
    terminal.draw(|f| ui.draw(f)).unwrap();
    let buf = terminal.backend().buffer();
    let rows: Vec<String> = (0..6)
        .map(|y| (0..60).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect();
    let hinted = rows
        .iter()
        .filter(|row: &&String| row.contains(SlashCommandPopup::HINT))
        .count();
    assert_eq!(hinted, 1, "{rows:?}");
    assert!(
        rows[3].contains(SlashCommandPopup::HINT) && rows[3].contains('╰'),
        "the hint belongs on the composer's bottom border: {rows:?}"
    );
}

#[test]
fn composer_hint_follows_focus_then_state() {
    fn input() -> InputView {
        InputView::new(Vec::new(), ProviderConfig::default())
    }

    assert_eq!(
        composer_hint(&input(), None, Keys::resting(true, false, Strip::Off)),
        Some(hint::BUSY)
    );
    assert_eq!(
        composer_hint(&input(), None, Keys::resting(false, false, Strip::Off)),
        Some(hint::IDLE)
    );
    assert_eq!(
        composer_hint(
            &input(),
            None,
            Keys::resting(false, true, Strip::Open { settled: false })
        ),
        Some(hint::ESC_ARMED),
        "the armed Esc overrides the strip hint"
    );

    let question = OverlayPrompt::Question {
        request_id: UserRequestId(1),
        popup: QuestionPrompt::new("which one?".into(), Vec::new()),
    };
    assert_eq!(
        composer_hint(
            &input(),
            Some(&question),
            Keys::resting(false, false, Strip::Open { settled: true })
        ),
        Some(QuestionPrompt::HINT),
        "the prompt states its own keys"
    );
    assert_eq!(
        composer_hint(
            &input(),
            Some(&answering_prompt()),
            Keys::resting(true, true, Strip::Open { settled: true })
        ),
        Some(hint::ANSWER)
    );
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

const TEST_PROVIDER: &str = "mock";
const TEST_API_KEY: &str = "test-key";
/// Nothing listens here, so the runtime's model listing fails offline.
const UNREACHABLE_BASE_URL: &str = "http://127.0.0.1:1/v1";
const TEST_QUESTION: &str = "which database?";
const TEST_ANSWER: &str = "postgres";
/// Builtin slash commands, in registration order. Down moves off the first.
const POPUP_FIRST: &str = "▸ /clear";
const POPUP_NEXT: &str = "▸ /compact";
const SCROLL_TAIL: &str = "line-39";

fn test_config() -> oven_app::config::AppConfig {
    use oven_app::config::{AppConfig, ProviderConfig, ProviderSelection};
    AppConfig {
        active_provider: ProviderSelection {
            name: TEST_PROVIDER.into(),
        },
        providers: [(
            TEST_PROVIDER.to_string(),
            ProviderConfig {
                name: Some(TEST_PROVIDER.into()),
                api_key: Some(TEST_API_KEY.into()),
                base_url: Some(UNREACHABLE_BASE_URL.into()),
                ..Default::default()
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    }
}

/// A `Ui` over a live runtime. The keyboard paths under test only reach the
/// command channel, so no provider ever answers.
async fn test_ui(root: &tempdir::TempDir) -> Ui {
    let app = oven_app::AppBuilder::new(root.path())
        .with_config(test_config())
        .await
        .open()
        .await
        .unwrap();
    Ui::new(app)
}

/// The question prompt after the user chose "Other…", so the composer owns
/// the answer.
fn answering_prompt() -> OverlayPrompt {
    let mut popup = QuestionPrompt::new(TEST_QUESTION.to_string(), Vec::new());
    let action = popup.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(action, QuestionPromptAction::Handled));
    assert!(popup.awaits_typed_answer());
    OverlayPrompt::Question {
        request_id: UserRequestId(1),
        popup,
    }
}

#[tokio::test]
async fn a_typed_answer_is_not_submitted_as_a_prompt() {
    let root = tempdir::TempDir::new("oven-ui-answer").unwrap();
    let mut ui = test_ui(&root).await;
    ui.prompt = Some(answering_prompt());
    ui.input.set_text(TEST_ANSWER);

    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));

    assert!(ui.prompt.is_none(), "the answer closes the question");
    assert!(ui.pending.is_empty(), "the answer must not be queued");
    assert!(
        ui.transcript.rewind_text().is_none(),
        "the answer must not start a transcript turn"
    );
}

#[tokio::test]
async fn a_typed_answer_is_not_queued_while_the_turn_runs() {
    let root = tempdir::TempDir::new("oven-ui-answer-busy").unwrap();
    let mut ui = test_ui(&root).await;
    ui.state.busy = true;
    ui.prompt = Some(answering_prompt());
    ui.input.set_text(TEST_ANSWER);

    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));

    assert!(
        ui.pending.is_empty(),
        "an answer is never a queued prompt, even mid-turn"
    );
    assert!(ui.prompt.is_none());
}

#[tokio::test]
async fn an_open_prompt_closes_when_its_request_resolves() {
    let root = tempdir::TempDir::new("oven-ui-resolved").unwrap();
    let mut ui = test_ui(&root).await;
    ui.prompt = Some(answering_prompt());

    ui.apply_event(&AppEvent::agent(AgentEvent::Tool(ToolEvent::Finished {
        call_id: ToolCallId(1),
        result: ToolResult::Cancelled,
        detail: None,
    })));
    ui.apply_event(&AppEvent::agent(AgentEvent::Turn(TurnEvent::Cancelled {
        duration_ms: 1,
    })));
    assert!(ui.prompt.is_some());

    ui.apply_event(&AppEvent::new(AppEventKind::RequestResolved {
        request_id: UserRequestId(9),
    }));
    assert!(ui.prompt.is_some());

    ui.apply_event(&AppEvent::new(AppEventKind::RequestResolved {
        request_id: UserRequestId(1),
    }));
    assert!(ui.prompt.is_none());
}

fn buffer_text(buf: &Buffer) -> String {
    let area = buf.area;
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn row_containing(buf: &Buffer, needle: &str) -> Option<String> {
    let area = buf.area;
    (0..area.height).find_map(|y| {
        let text: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
        text.contains(needle).then_some(text)
    })
}

fn row_fg(buf: &Buffer, needle: &str) -> Option<Color> {
    let area = buf.area;
    for y in 0..area.height {
        let text: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
        if !text.contains(needle) {
            continue;
        }
        return (0..area.width).find_map(|x| {
            let cell = &buf[(x, y)];
            (cell.symbol() != " ").then_some(cell.fg)
        });
    }
    None
}

fn row_reversed(buf: &Buffer, needle: &str) -> bool {
    let area = buf.area;
    for y in 0..area.height {
        let text: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
        if !text.contains(needle) {
            continue;
        }
        return (0..area.width).any(|x| buf[(x, y)].modifier.contains(Modifier::REVERSED));
    }
    false
}

#[tokio::test]
async fn arrows_then_enter_open_the_highlighted_subagent() {
    let root = tempdir::TempDir::new("oven-ui-agent-nav").unwrap();
    let mut ui = test_ui(&root).await;
    let agents = vec![running("alpha"), running("beta")];
    let beta = agents[1].id;
    ui.views.mirror(&agents);

    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(ui.views.picked_id(), Some(agents[0].id));
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
    terminal.draw(|f| ui.draw(f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let alpha = row_containing(&buf, "alpha").expect("alpha row");
    assert!(
        alpha.contains("running 0."),
        "elapsed counts from spawn, not the epoch: {alpha}"
    );
    assert!(
        row_reversed(&buf, "alpha"),
        "the highlighted row is drawn reversed"
    );
    assert_eq!(ui.views.picked_id(), Some(agents[0].id));

    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(ui.views.focused_id(), Some(beta));
    assert!(
        ui.transcript.rewind_text().is_none(),
        "opening a subagent must not send a prompt"
    );
}

#[tokio::test]
async fn the_strip_hint_offers_enter_only_after_a_row_is_highlighted() {
    let root = tempdir::TempDir::new("oven-ui-agent-hint").unwrap();
    let mut ui = test_ui(&root).await;
    ui.views.mirror(&[running("alpha")]);

    let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
    terminal.draw(|f| ui.draw(f)).unwrap();
    let before_buf = terminal.backend().buffer().clone();
    let before = buffer_text(&before_buf);
    assert!(
        row_reversed(&before_buf, &main_line(false)),
        "the driver starts selected: {before}"
    );
    assert_ne!(
        row_fg(&before_buf, "alpha"),
        Some(Color::Cyan),
        "an unselected row is not drawn in the accent color"
    );
    assert!(
        before.contains(hint::STRIP_SELECT),
        "arrows select before a row is highlighted: {before}"
    );
    assert!(
        !before.contains("enter view"),
        "enter is not live yet: {before}"
    );

    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    terminal.draw(|f| ui.draw(f)).unwrap();
    let after_buf = terminal.backend().buffer().clone();
    assert!(
        !row_reversed(&after_buf, &main_line(false)),
        "moving onto a subagent leaves the driver"
    );
    assert_eq!(
        row_fg(&after_buf, "alpha"),
        Some(Color::Cyan),
        "the highlight is the accent color"
    );
    let after = buffer_text(&after_buf);
    assert!(
        after.contains(hint::STRIP),
        "enter opens the highlighted row: {after}"
    );
}

#[tokio::test]
async fn up_wraps_around_the_strip() {
    let root = tempdir::TempDir::new("oven-ui-agent-wrap").unwrap();
    let mut ui = test_ui(&root).await;
    let agents = vec![running("alpha"), running("beta"), running("gamma")];
    let first = agents[0].id;
    let last = agents[2].id;
    ui.views.mirror(&agents);

    assert_eq!(
        ui.views.picked_id(),
        Some(ui.views.main_id()),
        "the driver is selected when the strip appears"
    );
    ui.handle_key(key(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(
        ui.views.picked_id(),
        Some(last),
        "up from the driver lands on the last row"
    );

    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(ui.views.picked_id(), Some(ui.views.main_id()));
    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(ui.views.picked_id(), Some(first));
    ui.handle_key(key(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(
        ui.views.picked_id(),
        Some(ui.views.main_id()),
        "up from the first subagent returns to the driver"
    );

    ui.handle_key(key(KeyCode::Up, KeyModifiers::NONE));
    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(ui.views.focused_id(), Some(last));
}

#[tokio::test]
async fn arrows_switch_the_open_subagent() {
    let root = tempdir::TempDir::new("oven-ui-agent-viewer-arrows").unwrap();
    let mut ui = test_ui(&root).await;
    let agents = vec![running("alpha"), running("beta")];
    let alpha = agents[0].id;
    let beta = agents[1].id;
    ui.views.mirror(&agents);
    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(ui.views.focused_id(), Some(beta));

    let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
    terminal.draw(|f| ui.draw(f)).unwrap();
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains(hint::VIEWER), "{text}");

    ui.handle_key(key(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(
        ui.views.focused_id(),
        Some(alpha),
        "up shows the previous agent"
    );

    ui.handle_key(key(KeyCode::Up, KeyModifiers::NONE));
    assert!(ui.views.focused_id().is_none());
    assert_eq!(
        ui.views.picked_id(),
        Some(ui.views.main_id()),
        "up from the first subagent returns to the driver"
    );
}

#[tokio::test]
async fn shift_up_scrolls_the_open_transcript() {
    let root = tempdir::TempDir::new("oven-ui-viewer-shift-up").unwrap();
    let mut ui = test_ui(&root).await;
    let agent = running("alpha");
    let id = agent.id;
    ui.views.mirror(std::slice::from_ref(&agent));
    ui.views.focus(id);
    let body = (0..40)
        .map(|i| format!("line-{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    ui.views
        .focused()
        .expect("the viewer is open")
        .push_shell_output(&body, true);

    let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
    terminal.draw(|f| ui.draw(f)).unwrap();
    let before = buffer_text(terminal.backend().buffer());
    assert!(before.contains(SCROLL_TAIL), "{before}");

    ui.handle_key(key(KeyCode::Up, KeyModifiers::SHIFT));

    assert_eq!(
        ui.views.focused_id(),
        Some(id),
        "shift-up scrolls the open transcript instead of switching agents"
    );
    terminal.draw(|f| ui.draw(f)).unwrap();
    let after = buffer_text(terminal.backend().buffer());
    assert_ne!(before, after, "a modified arrow scrolls; it is not dropped");
}

#[tokio::test]
async fn esc_leaves_the_subagent_openable_again() {
    let root = tempdir::TempDir::new("oven-ui-agent-reopen").unwrap();
    let mut ui = test_ui(&root).await;
    let agent = running("alpha");
    let id = agent.id;
    ui.views.mirror(&[agent]);

    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(ui.views.focused_id(), Some(id));

    ui.handle_key(esc());
    assert!(ui.views.focused_id().is_none());
    assert_eq!(
        ui.views.picked_id(),
        Some(ui.views.main_id()),
        "leaving the viewer selects the driver"
    );

    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(ui.views.focused_id(), Some(id));
}

#[tokio::test]
async fn a_settled_subagent_stays_openable_after_esc() {
    let root = tempdir::TempDir::new("oven-ui-agent-settled").unwrap();
    let mut ui = test_ui(&root).await;
    let mut agent = running("alpha");
    let id = agent.id;
    ui.views.mirror(std::slice::from_ref(&agent));
    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));

    agent.status = NodeStatus::Completed;
    agent.finished_at = Some(agent.started_at.saturating_add(10));
    ui.views.mirror(std::slice::from_ref(&agent));
    assert_eq!(
        ui.views.focused_id(),
        Some(id),
        "finishing stays in the viewer"
    );

    ui.handle_key(esc());
    assert!(ui.views.focused_id().is_none());
    assert!(
        ui.views.height() > 0,
        "the strip stays so it can be reopened"
    );

    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(ui.views.focused_id(), Some(id));
}

#[tokio::test]
async fn esc_again_hides_a_settled_strip() {
    let root = tempdir::TempDir::new("oven-ui-agent-dismiss").unwrap();
    let mut ui = test_ui(&root).await;
    let mut agent = running("alpha");
    ui.views.mirror(std::slice::from_ref(&agent));
    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    agent.status = NodeStatus::Completed;
    agent.finished_at = Some(agent.started_at.saturating_add(10));
    ui.views.mirror(std::slice::from_ref(&agent));

    ui.handle_key(esc());
    ui.handle_key(esc());

    assert_eq!(ui.views.height(), 0);
    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(ui.views.focused_id().is_none());
}

#[tokio::test]
async fn enter_without_a_highlight_does_not_open_a_subagent() {
    let root = tempdir::TempDir::new("oven-ui-agent-enter").unwrap();
    let mut ui = test_ui(&root).await;
    ui.views.mirror(&[running("alpha")]);

    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));

    assert!(ui.views.focused_id().is_none());
    assert!(ui.transcript.rewind_text().is_none());
}

#[tokio::test]
async fn a_draft_keeps_arrows_and_enter_for_the_composer() {
    let root = tempdir::TempDir::new("oven-ui-agent-draft").unwrap();
    let mut ui = test_ui(&root).await;
    ui.views.mirror(&[running("alpha")]);
    ui.input.set_text(TEST_ANSWER);

    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(
        ui.views.picked_id(),
        Some(ui.views.main_id()),
        "a draft keeps the arrows off the strip"
    );

    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));

    assert!(ui.views.focused_id().is_none());
    assert_eq!(ui.transcript.rewind_text().as_deref(), Some(TEST_ANSWER));
}

#[tokio::test]
async fn an_open_popup_keeps_arrows_and_enter() {
    let root = tempdir::TempDir::new("oven-ui-agent-popup").unwrap();
    let mut ui = test_ui(&root).await;
    ui.views.mirror(&[running("alpha")]);
    ui.input.set_text("/");
    assert_eq!(ui.input.overlay(), Overlay::Slash);
    assert!(!ui.strip_nav(), "an open popup owns ↑↓ and enter");

    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal.draw(|f| ui.draw(f)).unwrap();
    let before = buffer_text(terminal.backend().buffer());
    assert!(before.contains(POPUP_FIRST), "{before}");

    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(
        ui.views.picked_id(),
        Some(ui.views.main_id()),
        "the popup keeps the arrows"
    );
    terminal.draw(|f| ui.draw(f)).unwrap();
    let after = buffer_text(terminal.backend().buffer());
    assert!(
        after.contains(POPUP_NEXT),
        "down moves the completion selection: {after}"
    );

    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        ui.views.focused_id().is_none(),
        "enter fills the popup, it does not open a subagent"
    );
    terminal.draw(|f| ui.draw(f)).unwrap();
    let filled = buffer_text(terminal.backend().buffer());
    assert!(
        filled.contains("/compact"),
        "enter fills the highlighted command: {filled}"
    );
}

#[tokio::test]
async fn ordinary_text_still_starts_a_turn() {
    let root = tempdir::TempDir::new("oven-ui-prompt").unwrap();
    let mut ui = test_ui(&root).await;
    ui.input.set_text(TEST_ANSWER);

    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(ui.transcript.rewind_text().as_deref(), Some(TEST_ANSWER));
}

#[tokio::test]
async fn busy_follows_the_apps_phase_and_flushes_the_queue_when_it_ends() {
    let root = tempdir::TempDir::new("oven-ui-sync").unwrap();
    let mut ui = test_ui(&root).await;
    ui.state.busy = true;
    ui.pending.push(Queued {
        text: TEST_ANSWER.to_string(),
        steered: false,
    });

    ui.sync_state();

    assert!(!ui.state.busy, "an idle app is not busy");
    assert!(
        ui.pending.is_empty(),
        "the queue is sent once the app is idle"
    );
    assert_eq!(ui.transcript.rewind_text().as_deref(), Some(TEST_ANSWER));
}

#[tokio::test]
async fn a_prompt_already_appended_is_not_sent_again_when_idle() {
    let root = tempdir::TempDir::new("oven-ui-steer-flush").unwrap();
    let mut ui = test_ui(&root).await;
    ui.state.busy = true;
    ui.pending.push(Queued {
        text: TEST_ANSWER.to_string(),
        steered: true,
    });

    ui.sync_state();

    assert!(
        ui.pending.is_empty(),
        "a prompt the turn already took is dropped"
    );
    assert!(
        ui.transcript.rewind_text().is_none(),
        "flush must not start a second turn for it"
    );
}

#[tokio::test]
async fn an_appended_prompt_leaves_the_queue_and_joins_the_transcript() {
    let root = tempdir::TempDir::new("oven-ui-steer-event").unwrap();
    let mut ui = test_ui(&root).await;
    ui.state.busy = true;
    ui.pending.push(Queued {
        text: TEST_ANSWER.to_string(),
        steered: true,
    });

    ui.apply_event(&AppEvent::agent_with(
        ui.views.main_id(),
        TurnId(1),
        AgentEvent::Turn(TurnEvent::UserAppended {
            text: TEST_ANSWER.to_string(),
        }),
    ));

    assert!(ui.pending.is_empty(), "the queue row goes away");
    assert_eq!(ui.transcript.rewind_text().as_deref(), Some(TEST_ANSWER));
}

fn esc() -> KeyEvent {
    key(KeyCode::Esc, KeyModifiers::NONE)
}

/// A finished turn: its prompt is the rewindable row and nothing runs.
async fn finished_turn_ui(root: &tempdir::TempDir) -> Ui {
    let mut ui = test_ui(root).await;
    ui.input.set_text(TEST_ANSWER);
    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    ui.state.busy = false;
    ui
}

#[tokio::test]
async fn a_single_esc_only_arms_the_rewind() {
    let root = tempdir::TempDir::new("oven-ui-esc-arm").unwrap();
    let mut ui = finished_turn_ui(&root).await;

    ui.handle_key(esc());

    assert!(!ui.rewinding, "one press must not rewind the transcript");
    assert!(ui.esc_armed(), "the press arms the status bar hint");
    assert_eq!(
        ui.transcript.rewind_text().as_deref(),
        Some(TEST_ANSWER),
        "the prompt stays in the transcript until the confirm"
    );
}

#[tokio::test]
async fn a_second_esc_inside_the_window_rewinds() {
    let root = tempdir::TempDir::new("oven-ui-esc-rewind").unwrap();
    let mut ui = finished_turn_ui(&root).await;

    ui.handle_key(esc());
    ui.handle_key(esc());

    assert!(ui.rewinding, "the confirm rewinds the transcript");
    assert!(!ui.esc_armed(), "the confirm consumes the arm");
}

#[tokio::test]
async fn esc_on_a_settled_strip_confirms_a_rewind() {
    let root = tempdir::TempDir::new("oven-ui-esc-strip-rewind").unwrap();
    let mut ui = finished_turn_ui(&root).await;
    let mut agent = running("alpha");
    let id = agent.id;
    ui.views.mirror(std::slice::from_ref(&agent));
    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));
    agent.status = NodeStatus::Completed;
    agent.finished_at = Some(agent.started_at.saturating_add(10));
    ui.views.mirror(std::slice::from_ref(&agent));

    ui.handle_key(esc());
    assert!(ui.views.focused_id().is_none());
    assert!(ui.views.height() > 0);
    ui.handle_key(key(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(ui.views.picked_id(), Some(id));

    let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
    terminal.draw(|f| ui.draw(f)).unwrap();
    let before = buffer_text(terminal.backend().buffer());
    assert!(
        before.contains(hint::STRIP),
        "a rewindable message keeps esc on undo: {before}"
    );
    assert!(
        !before.contains(hint::STRIP_DONE),
        "esc close would skip the rewind confirm: {before}"
    );

    ui.handle_key(esc());

    assert!(
        ui.views.height() > 0,
        "the first Esc arms the rewind and leaves the strip"
    );
    assert!(ui.esc_armed());
    assert!(!ui.rewinding);
    assert!(
        ui.input.is_blank(),
        "the prompt stays in the transcript until the confirm"
    );
    terminal.draw(|f| ui.draw(f)).unwrap();
    let armed = buffer_text(terminal.backend().buffer());
    assert!(armed.contains(hint::ESC_ARMED), "{armed}");

    ui.handle_key(esc());

    assert!(ui.rewinding, "the confirm rewinds");
    assert!(!ui.input.is_blank(), "the confirm restores the prompt");
    assert!(
        ui.views.height() > 0,
        "rewinding does not dismiss the strip"
    );
}

#[tokio::test]
async fn an_expired_esc_arm_starts_over() {
    let root = tempdir::TempDir::new("oven-ui-esc-expired").unwrap();
    let mut ui = finished_turn_ui(&root).await;

    ui.handle_key(esc());
    ui.esc_confirm_until = Some(Instant::now() - Duration::from_millis(1));
    ui.expire_esc_confirm();
    assert!(!ui.esc_armed(), "the window closed in between");
    ui.handle_key(esc());

    assert!(!ui.rewinding, "the late press is too late to rewind");
    assert!(ui.esc_armed(), "it only armed the next pair");
}

#[tokio::test]
async fn any_other_key_drops_the_esc_arm() {
    let root = tempdir::TempDir::new("oven-ui-esc-interrupted").unwrap();
    let mut ui = finished_turn_ui(&root).await;

    ui.handle_key(esc());
    ui.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
    ui.handle_key(esc());

    assert!(!ui.rewinding, "typing broke the pair apart");
    assert!(ui.esc_armed(), "the second Esc is a first press again");
}

#[tokio::test]
async fn queued_text_waits_for_the_esc_confirm() {
    let root = tempdir::TempDir::new("oven-ui-esc-queue").unwrap();
    let mut ui = test_ui(&root).await;
    ui.state.busy = true;
    ui.pending.push(Queued {
        text: TEST_ANSWER.to_string(),
        steered: false,
    });

    ui.handle_key(esc());

    assert_eq!(ui.pending.len(), 1, "one press must not touch the queue");
    assert!(ui.esc_armed());

    ui.handle_key(esc());

    assert!(ui.pending.is_empty(), "the confirm pops the queue");
}

#[tokio::test]
async fn a_slash_prefix_cannot_open_a_completion_over_the_question() {
    let root = tempdir::TempDir::new("oven-ui-completion").unwrap();
    let mut ui = test_ui(&root).await;
    ui.prompt = Some(answering_prompt());

    for letter in ['/', 'm', 'o'] {
        ui.handle_key(key(KeyCode::Char(letter), KeyModifiers::NONE));
    }
    assert_eq!(
        ui.input.overlay(),
        Overlay::None,
        "a typed answer must not complete into a command"
    );

    ui.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(ui.input.overlay(), Overlay::None);
}

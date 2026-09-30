use super::*;
use crate::widgets::input::InputView;
use crate::widgets::question_prompt::{QuestionPrompt, QuestionPromptAction};
use crate::widgets::slash_command_popup::SlashCommandPopup;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use oven_app::config::ProviderConfig;
use oven_app::{AgentEvent, AppEventKind, ToolCallId, ToolEvent, ToolResult, TurnEvent};
use ratatui::backend::TestBackend;

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

    assert_eq!(composer_hint(&input(), true, None, false), Some(hint::BUSY));
    assert_eq!(
        composer_hint(&input(), false, None, false),
        Some(hint::IDLE)
    );
    assert_eq!(
        composer_hint(&input(), false, None, true),
        Some(hint::ESC_ARMED),
        "the armed Esc overrides the idle hint"
    );

    let question = OverlayPrompt::Question {
        request_id: UserRequestId(1),
        popup: QuestionPrompt::new("which one?".into(), Vec::new()),
    };
    assert_eq!(
        composer_hint(&input(), false, Some(&question), false),
        Some(QuestionPrompt::HINT),
        "the prompt states its own keys"
    );
    assert_eq!(
        composer_hint(&input(), true, Some(&answering_prompt()), true),
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
    ui.pending.push(TEST_ANSWER.to_string());

    ui.sync_state();

    assert!(!ui.state.busy, "an idle app is not busy");
    assert!(
        ui.pending.is_empty(),
        "the queue is sent once the app is idle"
    );
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
    ui.pending.push(TEST_ANSWER.to_string());

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

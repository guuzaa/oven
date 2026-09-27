use std::io;
use std::time::Duration;

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
};
use futures::StreamExt;
use oven_app::{
    AgentEvent, AnswerResponse, App, AppCommand, AppEvent, AppEventKind, ApprovalDecision,
    ApprovalRequestId, CompactionEvent, ControlCommand, LoopLimitDecision, LoopLimitRequestId,
    QuestionRequestId, ShellEvent, StateChange, StateEvent, ToolEvent, TurnEvent, invokes_command,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use tokio::sync::mpsc;

use crate::components::choice_popup::{ChoicePopup, ChoicePopupAction};
use crate::components::component::{Action, Component, KeyResult, State};
use crate::components::input::{InputView, Overlay, display_user_input};
use crate::components::paste_burst::{self, Burst};
use crate::components::question_prompt::{QuestionPrompt, QuestionPromptAction};
use crate::components::queue;
use crate::components::shell;
use crate::components::status::{StatusBar, StatusHint};
use crate::components::todos::TodosWidget;
use crate::components::transcript::Transcript;

use crate::components::{layout, terminal};

enum OverlayPrompt {
    Approval {
        request_id: ApprovalRequestId,
        popup: ChoicePopup,
    },
    LoopLimit {
        request_id: LoopLimitRequestId,
        popup: ChoicePopup,
    },
    Question {
        request_id: QuestionRequestId,
        popup: QuestionPrompt,
    },
}

impl OverlayPrompt {
    /// Rows the prompt wants; the question needs the width to wrap into them.
    fn height(&self, width: u16) -> u16 {
        match self {
            Self::Approval { popup, .. } | Self::LoopLimit { popup, .. } => popup.height(),
            Self::Question { popup, .. } => popup.height(width),
        }
    }

    fn draw(&self, f: &mut Frame<'_>, area: Rect) {
        match self {
            Self::Approval { popup, .. } | Self::LoopLimit { popup, .. } => popup.draw(f, area),
            Self::Question { popup, .. } => popup.draw(f, area),
        }
    }

    /// Whether the open question expects its answer typed into the input box.
    fn awaits_typed_answer(&self) -> bool {
        matches!(self, Self::Question { popup, .. } if popup.awaits_typed_answer())
    }
}

/// What an open overlay prompt does with a key.
enum PromptFlow {
    /// No prompt is open.
    Free,
    /// The prompt kept the key and stays open.
    Kept,
    /// The prompt is finished with: it approved, answered, declined or
    /// cancelled, so it closes.
    Closed,
    /// The open question expects its answer typed into the input box, which
    /// therefore owns the key.
    Typing,
}

pub struct Ui {
    app: App,
    events: mpsc::UnboundedReceiver<AppEvent>,
    state: State,
    quit: bool,
    /// Esc is ignored until `Rewound` arrives so a second rewind cannot
    /// desync the transcript from the backend.
    rewinding: bool,
    pending: Vec<String>,

    transcript: Transcript,
    status: StatusBar,
    input: InputView,
    todos: TodosWidget,
    prompt: Option<OverlayPrompt>,
}

impl Ui {
    pub fn new(app: App) -> Self {
        let events = app.subscribe();
        let slash_commands = app.slash_commands().to_vec();
        let model = app.model();
        let provider = app.provider_config();
        let root = app
            .root()
            .canonicalize()
            .unwrap_or_else(|_| app.root().to_owned());
        let last_turn_usage = app.last_turn_usage();
        let (context_tokens, context_window) = {
            let state = app.state();
            (state.context_tokens, state.context_window)
        };
        let todos = app.todos();
        let configured = app.configured_providers();
        let mut input = InputView::new(slash_commands, provider.clone()).with_root(&root);
        input.set_configured(configured.clone());
        if configured.is_empty() && provider.needs_setup() {
            input.open_setup();
        }
        Self {
            app,
            events,
            state: State::new(),
            quit: false,
            rewinding: false,
            pending: Vec::new(),

            transcript: Transcript::new(),
            status: StatusBar::new(model, &root, last_turn_usage)
                .with_effort(provider.reasoning_effort)
                .with_context(context_tokens, context_window),
            input,
            todos: TodosWidget::new(todos),
            prompt: None,
        }
    }

    /// Rebuilds the single scrollable transcript from backend history.
    fn reload_history(&mut self) {
        let mut transcript = Transcript::new();
        transcript.seed_timed(&self.app.history_timed_shared());
        self.transcript = transcript;
        self.rewinding = false;
    }

    pub async fn run(mut self) -> io::Result<()> {
        self.reload_history();
        let mut terminal = terminal::setup()?;
        let result = self.event_loop(&mut terminal).await;
        terminal::restore(&mut terminal)?;
        let session_id = self.app.session_id();
        self.app.shutdown().await;
        if let Some(id) = session_id {
            println!("oven -s {id}");
        }
        result
    }

    async fn event_loop(
        &mut self,
        terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    ) -> io::Result<()> {
        let mut term_events = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(80));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        terminal.draw(|f| self.draw(f))?;
        loop {
            tokio::select! {
                _ = tick.tick(), if self.wants_tick() => {
                    if self.state.busy {
                        self.state.frame = self.state.frame.wrapping_add(1);
                    }
                    self.status.expire_reply();
                }
                Some(ev) = term_events.next() => {
                    if self.handle_term_event(ev?)? {
                        break;
                    }
                }
                result = self.events.recv() => {
                    match result {
                        Some(ev) => self.apply_event(&ev),
                        None => self.disconnected(),
                    }
                    self.drain_events();
                    if self.quit {
                        break;
                    }
                }
            }
            terminal.draw(|f| self.draw(f))?;
        }
        Ok(())
    }

    /// Returns `true` when the app should quit.
    fn handle_term_event(&mut self, ev: Event) -> io::Result<bool> {
        match ev {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let (burst, trailing) = paste_burst::coalesce(key)?;
                match burst {
                    Burst::Key(key) => {
                        if self.handle_key(key) {
                            return Ok(true);
                        }
                    }
                    Burst::Paste(text) => {
                        self.input.paste(&text);
                        self.suppress_completions();
                    }
                }
                if let Some(ev) = trailing {
                    return self.handle_term_event(ev);
                }
            }
            Event::Paste(text) => {
                self.input.paste(&text);
                self.suppress_completions();
            }
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            _ => {}
        }
        Ok(false)
    }

    fn drain_events(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(ev) => self.apply_event(&ev),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.disconnected();
                    break;
                }
            }
        }
    }

    /// The backend went away mid-turn: close the response so an unfinished
    /// answer is settled before the next prompt archives the pinned one.
    fn disconnected(&mut self) {
        self.transcript.finish_response();
        self.state.busy = false;
    }

    fn apply_event(&mut self, ev: &AppEvent) {
        match &ev.kind {
            AppEventKind::Exited => self.quit = true,
            AppEventKind::Agent(env) => match &env.event {
                AgentEvent::Turn(TurnEvent::Started) => self.state.busy = true,
                AgentEvent::Turn(TurnEvent::LoopLimitReached {
                    request_id,
                    max_iters,
                }) => {
                    self.prompt = Some(OverlayPrompt::LoopLimit {
                        request_id: *request_id,
                        popup: ChoicePopup::loop_limit(*max_iters),
                    });
                }
                AgentEvent::Turn(
                    TurnEvent::Completed { .. }
                    | TurnEvent::Cancelled { .. }
                    | TurnEvent::Failed { .. },
                ) => {
                    self.state.busy = false;
                    self.prompt = None;
                }
                AgentEvent::Tool(ToolEvent::ApprovalRequested {
                    request_id,
                    name,
                    view,
                    ..
                }) => {
                    self.prompt = Some(OverlayPrompt::Approval {
                        request_id: *request_id,
                        popup: ChoicePopup::approval(name, &view.summary),
                    });
                }
                AgentEvent::Tool(ToolEvent::QuestionAsked {
                    request_id,
                    question,
                }) => {
                    self.prompt = Some(OverlayPrompt::Question {
                        request_id: *request_id,
                        popup: QuestionPrompt::new(
                            question.question.clone(),
                            question.options.clone(),
                        ),
                    });
                }
                AgentEvent::Tool(ToolEvent::Finished { .. }) => self.prompt = None,
                _ => {}
            },
            AppEventKind::Shell(ev) => match ev {
                ShellEvent::Started { .. } => self.state.busy = true,
                ShellEvent::Finished { .. } | ShellEvent::Failed { .. } => {
                    self.state.busy = false;
                }
            },
            AppEventKind::Compaction(ev) => {
                self.state.busy = matches!(ev, CompactionEvent::Started);
            }
            AppEventKind::StateChanged(StateEvent { change, .. }) => match change {
                StateChange::ModeChanged { mode } => self.state.mode = *mode,
                StateChange::HistoryChanged { .. } => self.reload_history(),
                _ => {}
            },
            AppEventKind::Notification { .. } | AppEventKind::Error { .. } => {
                if !self.app.state().phase.is_active() {
                    self.state.busy = false;
                }
            }
        }
        self.transcript.on_event(ev);
        self.status.on_event(ev);
        self.input.on_event(ev);
        self.todos.on_event(ev);
        self.maybe_flush();
    }

    fn maybe_flush(&mut self) {
        if self.state.busy || self.pending.is_empty() {
            return;
        }
        let texts = std::mem::take(&mut self.pending);
        self.state.busy = true;
        let remaining = send_each(texts, |text| {
            if self.app.send(AppCommand::Prompt(text.to_string())).is_ok() {
                self.push_submitted(text);
                true
            } else {
                false
            }
        });
        if !remaining.is_empty() {
            let mut rest = remaining;
            rest.append(&mut self.pending);
            self.pending = rest;
            self.state.busy = false;
        }
    }

    /// A submitted turn enters the transcript before response events, so its
    /// prompt and response always share one scroll coordinate system.
    fn push_submitted(&mut self, text: &str) {
        match classify_prompt_for_display(text) {
            PromptDisplay::User(text) => self.transcript.start_user_turn(&text),
            PromptDisplay::Shell(command) => self.transcript.start_shell_turn(&command),
            PromptDisplay::Quiet => {}
        }
    }

    fn control(&self, command: ControlCommand) {
        let _ = self.app.send(AppCommand::Control(command));
    }

    fn send_cancel(&self) {
        if let Some(turn_id) = self.app.state().phase.turn_id() {
            self.control(ControlCommand::Cancel { turn_id });
        }
    }

    /// Gives the open overlay prompt first refusal on `key`.
    ///
    /// The prompt is taken out of `self` so the decision it produces can be
    /// sent with `&mut self`, then put back unless it is finished with.
    fn handle_prompt_key(&mut self, key: KeyEvent) -> PromptFlow {
        let Some(mut prompt) = self.prompt.take() else {
            return PromptFlow::Free;
        };
        let flow = match &mut prompt {
            OverlayPrompt::Approval { request_id, popup } => match popup.handle_key(key) {
                ChoicePopupAction::Handled => PromptFlow::Kept,
                ChoicePopupAction::Confirm(row) => {
                    let decision = if row == 0 {
                        ApprovalDecision::Approved
                    } else {
                        ApprovalDecision::Rejected
                    };
                    self.control(ControlCommand::RespondToolApproval {
                        request_id: *request_id,
                        decision,
                    });
                    PromptFlow::Closed
                }
                ChoicePopupAction::Cancel => {
                    self.send_cancel();
                    PromptFlow::Closed
                }
            },
            OverlayPrompt::LoopLimit { request_id, popup } => match popup.handle_key(key) {
                ChoicePopupAction::Handled => PromptFlow::Kept,
                ChoicePopupAction::Confirm(row) => {
                    let decision = if row == 0 {
                        LoopLimitDecision::Continue
                    } else {
                        LoopLimitDecision::Exit
                    };
                    self.control(ControlCommand::RespondLoopLimit {
                        request_id: *request_id,
                        decision,
                    });
                    PromptFlow::Closed
                }
                ChoicePopupAction::Cancel => {
                    self.send_cancel();
                    PromptFlow::Closed
                }
            },
            OverlayPrompt::Question { request_id, popup } => match popup.handle_key(key) {
                QuestionPromptAction::Handled => PromptFlow::Kept,
                QuestionPromptAction::Typing => PromptFlow::Typing,
                QuestionPromptAction::Answered(answer) => {
                    self.respond_question(*request_id, AnswerResponse::Answered { answer });
                    PromptFlow::Closed
                }
                QuestionPromptAction::Declined => {
                    self.respond_question(*request_id, AnswerResponse::Declined);
                    PromptFlow::Closed
                }
                QuestionPromptAction::Cancelled => {
                    self.send_cancel();
                    PromptFlow::Closed
                }
            },
        };
        if matches!(flow, PromptFlow::Kept | PromptFlow::Typing) {
            self.prompt = Some(prompt);
        }
        flow
    }

    /// Keeps the composer's completion popups shut while an answer is being
    /// typed: they draw under the question prompt, and Tab would otherwise
    /// complete command text straight into the answer.
    fn suppress_completions(&mut self) {
        if self
            .prompt
            .as_ref()
            .is_some_and(OverlayPrompt::awaits_typed_answer)
        {
            self.input.close_popups();
        }
    }

    fn respond_question(&self, request_id: QuestionRequestId, response: AnswerResponse) {
        self.control(ControlCommand::RespondQuestion {
            request_id,
            response,
        });
    }

    /// Routes submitted text to the open question when it is waiting for a
    /// typed answer, returning whether the text was consumed as that answer.
    fn answer_question_with(&mut self, text: &str) -> bool {
        let Some(OverlayPrompt::Question { request_id, popup }) = self.prompt.as_ref() else {
            return false;
        };
        if !popup.awaits_typed_answer() {
            return false;
        }
        let request_id = *request_id;
        self.prompt = None;
        self.respond_question(
            request_id,
            AnswerResponse::Answered {
                answer: text.to_string(),
            },
        );
        true
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match self.transcript.handle_mouse(mouse, &self.state) {
            KeyResult::Action(Action::Notify(text)) => {
                self.apply_event(&AppEvent::notification(text));
            }
            KeyResult::Ignored => {
                self.input.handle_mouse(mouse, &self.state);
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        let result = match self.handle_prompt_key(key) {
            PromptFlow::Kept | PromptFlow::Closed => return false,
            PromptFlow::Typing => {
                let result = self.input.handle_key(key, &self.state);
                self.suppress_completions();
                result
            }
            PromptFlow::Free => match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    KeyResult::Action(Action::Quit)
                }
                _ if is_mode_toggle(key) => {
                    self.state.mode = self.state.mode.toggle();
                    self.control(ControlCommand::SetMode {
                        mode: self.state.mode,
                    });
                    KeyResult::Handled
                }
                KeyCode::Esc if self.input.overlay() == Overlay::None => match EscAction::new(
                    self.pending.pop(),
                    self.state.busy,
                    self.rewinding,
                    self.transcript.rewind_text(),
                ) {
                    EscAction::PopQueue(text) => {
                        self.input.set_text(&text);
                        KeyResult::Handled
                    }
                    EscAction::Cancel => KeyResult::Action(Action::Cancel),
                    EscAction::Rewind(text) => {
                        self.input.set_text(&text);
                        self.rewinding = true;
                        if self
                            .app
                            .send(AppCommand::Control(ControlCommand::Rewind))
                            .is_err()
                        {
                            self.rewinding = false;
                        }
                        KeyResult::Handled
                    }
                    EscAction::Ignore => KeyResult::Handled,
                },
                // Plain Enter during rewind would submit before history is truncated.
                KeyCode::Enter if self.rewinding && key.modifiers.is_empty() => KeyResult::Handled,
                _ => match self.transcript.handle_key(key, &self.state) {
                    KeyResult::Ignored => self.input.handle_key(key, &self.state),
                    other => other,
                },
            },
        };

        match result {
            KeyResult::Ignored | KeyResult::Handled => false,
            KeyResult::Action(Action::Quit) => {
                if self.state.busy {
                    self.send_cancel();
                }
                true
            }
            KeyResult::Action(Action::Cancel) => {
                self.send_cancel();
                false
            }
            KeyResult::Action(Action::Queue(text)) => {
                if !self.answer_question_with(&text) {
                    self.pending.push(text);
                }
                false
            }
            KeyResult::Action(Action::Submit(text)) => {
                if self.answer_question_with(&text) {
                    return false;
                }
                self.push_submitted(&text);
                self.status.clear_reply();
                self.input.clear();
                self.state.busy = true;
                if self.app.send(AppCommand::Prompt(text)).is_err() {
                    self.state.busy = false;
                }
                false
            }
            KeyResult::Action(Action::QuietSubmit(text)) => {
                if !self.answer_question_with(&text) {
                    let _ = self.app.send(AppCommand::Prompt(text));
                }
                false
            }
            KeyResult::Action(Action::Notify(text)) => {
                self.apply_event(&AppEvent::notification(text));
                false
            }
        }
    }

    fn draw(&mut self, f: &mut Frame<'_>) {
        let area = f.area();
        let overlay_height = match self.prompt.as_ref() {
            Some(prompt) => prompt.height(area.width),
            None => self.input.overlay_height(),
        };
        let regions = layout::split(
            area,
            self.input.height(area.width),
            queue::height(&self.pending),
            self.todos.height(),
            overlay_height,
        );

        self.transcript.draw(f, regions.transcript, &self.state);
        if let Some(queue) = regions.queue {
            queue::draw(f, queue, &self.pending);
        }
        if let Some(todos) = regions.todos {
            self.todos.draw(f, todos);
        }
        self.input.draw(f, regions.input, &self.state);
        if let Some(overlay) = regions.overlay {
            match self.prompt.as_ref() {
                Some(prompt) => prompt.draw(f, overlay),
                None => self.input.draw_overlay(f, overlay),
            }
        }
        self.status.draw_bar(
            f,
            regions.status,
            &self.state,
            status_hint(self.input.overlay(), self.state.busy, self.prompt.as_ref()),
        );
        self.status.draw_reply_overlay(f, regions.transcript);
    }

    fn wants_tick(&self) -> bool {
        self.state.busy || self.status.has_reply()
    }
}

/// Classifies submitted text by the kind of turn it starts. Control commands
/// do not create transcript rows.
enum PromptDisplay {
    User(String),
    Shell(String),
    Quiet,
}

fn classify_prompt_for_display(text: &str) -> PromptDisplay {
    if let Some(command) = shell::command(text) {
        return PromptDisplay::Shell(command.to_string());
    }
    if invokes_command(text) {
        return PromptDisplay::Quiet;
    }
    PromptDisplay::User(display_user_input(text))
}

fn is_mode_toggle(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::BackTab)
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
}

fn status_hint(overlay: Overlay, busy: bool, prompt: Option<&OverlayPrompt>) -> StatusHint {
    match prompt {
        Some(OverlayPrompt::Approval { .. }) => return StatusHint::Approval,
        Some(OverlayPrompt::LoopLimit { .. }) => return StatusHint::LoopLimit,
        Some(prompt) if prompt.awaits_typed_answer() => return StatusHint::AnswerTyping,
        Some(OverlayPrompt::Question { .. }) => return StatusHint::Question,
        None => {}
    }
    match overlay {
        Overlay::Slash | Overlay::Mention => StatusHint::Slash,
        Overlay::Model | Overlay::Setup => StatusHint::Modal,
        Overlay::None if busy => StatusHint::Busy,
        Overlay::None => StatusHint::Idle,
    }
}

enum EscAction {
    PopQueue(String),
    Cancel,
    Rewind(String),
    Ignore,
}

impl EscAction {
    fn new(queued: Option<String>, busy: bool, rewinding: bool, last_user: Option<String>) -> Self {
        if let Some(text) = queued {
            return EscAction::PopQueue(text);
        }
        if busy {
            return EscAction::Cancel;
        }
        if rewinding {
            return EscAction::Ignore;
        }
        match last_user {
            Some(text) => EscAction::Rewind(text),
            None => EscAction::Ignore,
        }
    }
}

fn send_each(texts: Vec<String>, mut send: impl FnMut(&str) -> bool) -> Vec<String> {
    let mut iter = texts.into_iter();
    let mut remaining = Vec::new();
    while let Some(text) = iter.next() {
        if !send(&text) {
            remaining.push(text);
            remaining.extend(iter);
            break;
        }
    }
    remaining
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_action_priority_queue_then_cancel_then_rewind() {
        assert!(matches!(
            EscAction::new(Some("q".into()), true, false, Some("u".into())),
            EscAction::PopQueue(t) if t == "q"
        ));
        assert!(matches!(
            EscAction::new(Some("q".into()), true, true, None),
            EscAction::PopQueue(t) if t == "q"
        ));
        assert!(matches!(
            EscAction::new(None, true, false, Some("u".into())),
            EscAction::Cancel
        ));
        assert!(matches!(
            EscAction::new(None, false, true, Some("u".into())),
            EscAction::Ignore
        ));
        assert!(matches!(
            EscAction::new(None, false, false, Some("u".into())),
            EscAction::Rewind(t) if t == "u"
        ));
        assert!(matches!(
            EscAction::new(None, false, false, None),
            EscAction::Ignore
        ));
    }

    #[test]
    fn empty_prompt_cannot_trigger_rewind() {
        assert!(matches!(
            EscAction::new(None, false, false, None),
            EscAction::Ignore
        ));
    }

    #[test]
    fn classify_starts_user_turn_for_ordinary_text() {
        assert!(matches!(
            classify_prompt_for_display("why is the build slow?"),
            PromptDisplay::User(text) if text == "why is the build slow?"
        ));
    }

    #[test]
    fn classify_starts_shell_turn_for_shell_commands() {
        assert!(matches!(
            classify_prompt_for_display("! ls -la"),
            PromptDisplay::Shell(text) if text == "ls -la"
        ));
    }

    #[test]
    fn classify_starts_user_turn_for_unknown_slash_commands() {
        assert!(matches!(
            classify_prompt_for_display("/nope"),
            PromptDisplay::User(text) if text == "/nope"
        ));
    }

    #[test]
    fn classify_keeps_control_commands_out_of_the_transcript() {
        for text in [
            "/clear",
            "/compact",
            "/exit",
            "/model",
            "/model gpt-4o high",
            "/plan on",
            "/setup name=deepseek api_key=sk-secret",
        ] {
            assert!(
                matches!(classify_prompt_for_display(text), PromptDisplay::Quiet),
                "{text} configures the runtime and produces no response"
            );
        }
    }

    #[test]
    fn classified_setup_command_never_starts_a_turn() {
        assert!(matches!(
            classify_prompt_for_display("/setup name=deepseek api_key=sk-secret"),
            PromptDisplay::Quiet
        ));
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

    #[test]
    fn status_hint_follows_overlay_then_busy() {
        assert_eq!(status_hint(Overlay::Slash, true, None), StatusHint::Slash);
        assert_eq!(status_hint(Overlay::Setup, false, None), StatusHint::Modal);
        assert_eq!(status_hint(Overlay::Model, false, None), StatusHint::Modal);
        assert_eq!(status_hint(Overlay::None, true, None), StatusHint::Busy);
        assert_eq!(status_hint(Overlay::None, false, None), StatusHint::Idle);
        let approval = OverlayPrompt::Approval {
            request_id: ApprovalRequestId(1),
            popup: ChoicePopup::approval("bash", "run ls"),
        };
        assert_eq!(
            status_hint(Overlay::None, true, Some(&approval)),
            StatusHint::Approval
        );
        let loop_limit = OverlayPrompt::LoopLimit {
            request_id: LoopLimitRequestId(1),
            popup: ChoicePopup::loop_limit(100),
        };
        assert_eq!(
            status_hint(Overlay::None, true, Some(&loop_limit)),
            StatusHint::LoopLimit
        );
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn is_mode_toggle_backtab_and_shift_tab() {
        assert!(is_mode_toggle(key(KeyCode::BackTab, KeyModifiers::NONE)));
        assert!(is_mode_toggle(key(KeyCode::Tab, KeyModifiers::SHIFT)));
        assert!(!is_mode_toggle(key(KeyCode::Tab, KeyModifiers::NONE)));
        assert!(!is_mode_toggle(key(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
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
            request_id: QuestionRequestId(1),
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
    async fn ordinary_text_still_starts_a_turn() {
        let root = tempdir::TempDir::new("oven-ui-prompt").unwrap();
        let mut ui = test_ui(&root).await;
        ui.input.set_text(TEST_ANSWER);

        ui.handle_key(key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(ui.transcript.rewind_text().as_deref(), Some(TEST_ANSWER));
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
}

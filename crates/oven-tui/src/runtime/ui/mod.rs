use std::collections::BTreeMap;
use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use futures::StreamExt;
use oven_app::{
    AgentEvent, AgentId, AnswerResponse, App, AppEvent, AppEventKind, AppState, ApprovalDecision,
    Input, LoopLimitDecision, NodeInfo, SubagentEvent, ToolEvent, TurnEvent, UserRequestId,
    UserResponse,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use tokio::sync::{mpsc, watch};

use crate::core::component::{Action, Component, KeyResult, State};
use crate::core::esc::{ESC_CONFIRM_WINDOW, EscAction};
use crate::core::paste::{self, Burst};
use crate::widgets::agents;
use crate::widgets::choice_popup::{ChoicePopup, ChoicePopupAction};
use crate::widgets::input::{InputView, Overlay, display_user_input};
use crate::widgets::question_prompt::{QuestionPrompt, QuestionPromptAction};
use crate::widgets::queue;
use crate::widgets::status::StatusBar;
use crate::widgets::todos::TodosWidget;
use crate::widgets::transcript::Transcript;

use crate::core::hint::{self, Prompt};
use crate::core::layout;
use crate::platform::terminal;

/// Lines an arrow key scrolls a subagent's transcript by.
const VIEWER_SCROLL_LINES: u16 = 1;
/// The viewer replaces the composer with a single hint row.
const VIEWER_ROWS: u16 = 1;

enum OverlayPrompt {
    Approval {
        request_id: UserRequestId,
        popup: ChoicePopup,
    },
    LoopLimit {
        request_id: UserRequestId,
        popup: ChoicePopup,
    },
    Question {
        request_id: UserRequestId,
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

    fn request_id(&self) -> UserRequestId {
        match self {
            Self::Approval { request_id, .. }
            | Self::LoopLimit { request_id, .. }
            | Self::Question { request_id, .. } => *request_id,
        }
    }

    /// The keys the open prompt answers.
    fn hint(&self) -> &'static str {
        match self {
            Self::Approval { popup, .. } | Self::LoopLimit { popup, .. } => popup.hint(),
            Self::Question { .. } => QuestionPrompt::HINT,
        }
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
    /// The app's levels — phase, mode, model, subagents — which the UI reads
    /// as they stand rather than replaying what changed.
    state_rx: watch::Receiver<AppState>,
    state: State,
    quit: bool,
    /// Esc is ignored until `Rewound` arrives so a second rewind cannot
    /// desync the transcript from the backend.
    rewinding: bool,
    pending: Vec<String>,
    /// Deadline for the Esc press that confirms a previous one.
    esc_confirm_until: Option<Instant>,

    transcript: Transcript,
    /// One transcript per subagent, built from the events it reports. The
    /// driver's own conversation stays in `transcript`.
    views: BTreeMap<AgentId, Transcript>,
    /// The driver. Anything else is a subagent, and its events belong to
    /// `views`.
    main_agent: AgentId,
    agents: Vec<NodeInfo>,
    /// The subagent whose transcript has taken over the screen.
    focus: Option<AgentId>,
    /// Where the strip was drawn, so a click can be mapped back to a row.
    agents_area: Option<Rect>,

    status: StatusBar,
    input: InputView,
    todos: TodosWidget,
    prompt: Option<OverlayPrompt>,
}

impl Ui {
    pub fn new(app: App) -> Self {
        let events = app.subscribe();
        let state_rx = app.watch_state();
        let snapshot = app.state();
        let root = app
            .root()
            .canonicalize()
            .unwrap_or_else(|_| app.root().to_owned());
        let mut input =
            InputView::new(app.slash_commands(), snapshot.provider.clone()).with_root(&root);
        input.sync(&snapshot);
        if snapshot.configured_providers.is_empty() && snapshot.provider.needs_setup() {
            input.open_setup();
        }
        let state = State {
            busy: snapshot.phase.is_active(),
            agents: active_agents(&snapshot.subagents),
            mode: snapshot.mode,
            ..State::new()
        };
        Self {
            app,
            events,
            state_rx,
            state,
            quit: false,
            rewinding: false,
            pending: Vec::new(),
            esc_confirm_until: None,

            transcript: Transcript::new(),
            views: BTreeMap::new(),
            main_agent: snapshot.agent_id,
            agents: snapshot.subagents.to_vec(),
            focus: None,
            agents_area: None,
            status: StatusBar::new(snapshot.model.clone(), &root, snapshot.last_turn_usage)
                .with_effort(snapshot.reasoning_effort)
                .with_context(snapshot.context_tokens, snapshot.context_window),
            input,
            todos: TodosWidget::new(snapshot.todos.clone()),
            prompt: None,
        }
    }

    /// Rebuilds the single scrollable transcript from backend history.
    fn reload_history(&mut self) {
        let mut transcript = Transcript::new();
        transcript.seed_timed(&self.state_rx.borrow().history_timed_shared());
        self.transcript = transcript;
        self.rewinding = false;
    }

    pub async fn run(mut self) -> io::Result<()> {
        self.reload_history();
        let mut terminal = terminal::setup()?;
        let result = self.event_loop(&mut terminal).await;
        terminal::restore(&mut terminal)?;
        // Whatever the user queued and never sent dies with the app, so say
        // so where they can still read it: the alternate screen is gone by
        // now, and a notice drawn a moment before quitting would not be.
        let queued = std::mem::take(&mut self.pending);
        let session_id = self.app.session_id();
        let unsent = self.app.shutdown().await + queued.len();
        if unsent > 0 {
            println!("{}", unsent_notice(unsent));
        }
        if let Some(id) = session_id {
            println!("oven -s {id}");
        }
        result
    }

    async fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> io::Result<()> {
        let mut term_events = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(80));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        self.draw_frame(terminal)?;
        loop {
            tokio::select! {
                _ = tick.tick(), if self.wants_tick() => {
                    if self.state.busy {
                        self.state.frame = self.state.frame.wrapping_add(1);
                    }
                    self.status.expire_reply();
                    self.expire_esc_confirm();
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
                Ok(()) = self.state_rx.changed() => self.sync_state(),
            }
            self.draw_frame(terminal)?;
        }
        Ok(())
    }

    /// Draws a frame, then puts the hardware cursor away: the composer parks it
    /// on the caret so terminals anchor their IME composition window there, and
    /// the caret itself is the cell the composer draws.
    fn draw_frame(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
        terminal.draw(|f| self.draw(f))?;
        terminal.hide_cursor()
    }

    /// Returns `true` when the app should quit.
    fn handle_term_event(&mut self, ev: Event) -> io::Result<bool> {
        match ev {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let (burst, trailing) = paste::coalesce(key)?;
                match burst {
                    Burst::Key(key) => {
                        if self.handle_key(key) {
                            return Ok(true);
                        }
                    }
                    Burst::Paste(text) => {
                        self.clear_esc_confirm();
                        self.input.paste(&text);
                        self.suppress_completions();
                    }
                }
                if let Some(ev) = trailing {
                    return self.handle_term_event(ev);
                }
            }
            Event::Paste(text) => {
                self.clear_esc_confirm();
                self.input.paste(&text);
                self.suppress_completions();
            }
            Event::Mouse(mouse) => self.handle_mouse(mouse, self.agents_area),
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
            AppEventKind::Subagent(SubagentEvent::Focus { id }) => self.focus_agent(*id),
            // A subagent's turn is not the driver's: its events feed its own
            // transcript and nothing else. Only the notification that it
            // finished reaches the composer and the status bar.
            AppEventKind::Agent(env) if env.agent_id != self.main_agent => {
                self.views
                    .entry(env.agent_id)
                    .or_insert_with(Transcript::new)
                    .on_event(ev);
                return;
            }
            AppEventKind::Agent(env) => match &env.event {
                AgentEvent::Turn(TurnEvent::LoopLimitReached {
                    request_id,
                    max_iters,
                }) => {
                    self.prompt = Some(OverlayPrompt::LoopLimit {
                        request_id: *request_id,
                        popup: ChoicePopup::loop_limit(*max_iters),
                    });
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
                _ => {}
            },
            AppEventKind::RequestResolved { request_id } => {
                if self
                    .prompt
                    .as_ref()
                    .is_some_and(|prompt| prompt.request_id() == *request_id)
                {
                    self.prompt = None;
                }
            }
            AppEventKind::HistoryChanged { .. } => self.reload_history(),
            AppEventKind::Shell(_)
            | AppEventKind::Compaction(_)
            | AppEventKind::Notification { .. }
            | AppEventKind::Error { .. } => {}
        }
        self.transcript.on_event(ev);
        self.status.on_event(ev);
        self.todos.on_event(ev);
    }

    /// Follows the levels the app reports. What the driver is doing is read
    /// from its phase, so the composer is busy exactly while the app is.
    fn sync_state(&mut self) {
        let subagents = {
            let state = self.state_rx.borrow_and_update();
            self.state.busy = state.phase.is_active();
            self.state.mode = state.mode;
            self.status.sync(&state);
            self.input.sync(&state);
            self.todos.sync(&state);
            Arc::clone(&state.subagents)
        };
        self.on_subagents(&subagents);
        self.maybe_flush();
    }

    /// Opens a subagent's transcript, building it on first view.
    ///
    /// A subagent that has not said anything yet — one waiting for a slot, or
    /// one just spawned — has no transcript of its own, and opening nothing
    /// is worse than opening a page that says what it was asked to do. The
    /// task's label is that page: its events fill in under it as they arrive.
    fn focus_agent(&mut self, id: AgentId) {
        if !self.views.contains_key(&id) {
            let mut view = Transcript::new();
            match self.agents.iter().find(|agent| agent.id == id) {
                Some(agent) if !agent.label.is_empty() => view.start_user_turn(&agent.label),
                _ => {}
            }
            self.views.insert(id, view);
        }
        self.focus = Some(id);
    }

    /// Mirrors the registry into the strip and drops what it no longer
    /// holds: a view of a subagent nobody can reach is only memory.
    fn on_subagents(&mut self, subagents: &[NodeInfo]) {
        self.agents.clear();
        self.agents.extend_from_slice(subagents);
        self.state.agents = active_agents(subagents);
        self.views
            .retain(|id, _| self.agents.iter().any(|agent| agent.id == *id));
        if self.focus.is_some_and(|id| !self.views.contains_key(&id)) {
            self.focus = None;
        }
    }

    fn maybe_flush(&mut self) {
        if self.state.busy || self.pending.is_empty() {
            return;
        }
        let texts = std::mem::take(&mut self.pending);
        let remaining = send_each(texts, |text| {
            self.app
                .submit(text)
                .inspect(|input| self.push_submitted(input))
                .is_ok()
        });
        if !remaining.is_empty() {
            let mut rest = remaining;
            rest.append(&mut self.pending);
            self.pending = rest;
        }
    }

    /// A submitted turn enters the transcript before response events, so its
    /// prompt and response always share one scroll coordinate system.
    fn push_submitted(&mut self, input: &Input) {
        match input {
            Input::Chat(text) => self.transcript.start_user_turn(&display_user_input(text)),
            Input::Shell(command) if !command.is_empty() => {
                self.transcript.start_shell_turn(command);
            }
            Input::Shell(_) | Input::Slash { .. } | Input::Rewind => {}
        }
    }

    /// Stops what is still running before the process goes away: the turn
    /// first, then the subagents it may have spawned. `App::shutdown` cancels
    /// the supervisor as a backstop, but a subagent that is asked to stop is
    /// stopped in the registry too, which is what the last frame shows.
    fn shutdown_in_flight(&self) {
        if self.state.busy {
            self.send_cancel();
        }
        if !self.agents.is_empty() {
            self.app.stop_subagents();
        }
    }

    fn send_cancel(&self) {
        let turn_id = self.state_rx.borrow().phase.turn_id();
        if let Some(turn_id) = turn_id {
            self.app.cancel(turn_id);
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
                    self.app
                        .respond(*request_id, UserResponse::Approval(decision));
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
                    self.app
                        .respond(*request_id, UserResponse::LoopLimit(decision));
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

    fn respond_question(&self, request_id: UserRequestId, response: AnswerResponse) {
        self.app.respond(request_id, UserResponse::Answer(response));
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

    fn handle_mouse(&mut self, mouse: MouseEvent, agents_area: Option<Rect>) {
        if let Some(area) = agents_area
            && let Some(id) = agents::row_at(area, &self.agents, mouse.row)
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            self.focus_agent(id);
            return;
        }
        // Whichever transcript is on screen takes the mouse: sending it to the
        // hidden one would scroll what nobody can see, and copy the wrong text
        // to the clipboard.
        let result = match self.focus.and_then(|id| self.views.get_mut(&id)) {
            Some(view) => view.handle_mouse(mouse, &self.state),
            None => self.transcript.handle_mouse(mouse, &self.state),
        };
        match result {
            KeyResult::Action(Action::Notify(text)) => {
                self.apply_event(&AppEvent::notification(text));
            }
            // The composer is not drawn while a viewer is open, so there is
            // nothing under the mouse there to hand the event to either.
            KeyResult::Ignored if self.focus.is_none() => {
                self.input.handle_mouse(mouse, &self.state);
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        let esc_armed = self.esc_armed();
        self.clear_esc_confirm();
        let result = match self.handle_prompt_key(key) {
            PromptFlow::Kept | PromptFlow::Closed => return false,
            PromptFlow::Typing => {
                let result = self.input.handle_key(key, &self.state);
                self.suppress_completions();
                result
            }
            PromptFlow::Free if self.focus.is_some() => self.handle_viewer_key(key, esc_armed),
            PromptFlow::Free => match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    KeyResult::Action(Action::Quit)
                }
                _ if is_mode_toggle(key) => {
                    self.state.mode = self.state.mode.toggle();
                    self.app.set_mode(self.state.mode);
                    KeyResult::Handled
                }
                KeyCode::Esc if self.input.overlay() == Overlay::None => self.handle_esc(esc_armed),
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
                self.shutdown_in_flight();
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
                self.status.clear_reply();
                self.input.clear();
                if let Ok(input) = self.app.submit(&text) {
                    self.push_submitted(&input);
                }
                false
            }
            KeyResult::Action(Action::QuietSubmit(text)) => {
                if !self.answer_question_with(&text) {
                    let _ = self.app.submit(&text);
                }
                false
            }
            KeyResult::Action(Action::Notify(text)) => {
                self.apply_event(&AppEvent::notification(text));
                false
            }
        }
    }

    /// A subagent's transcript has the keyboard while it is open: the
    /// composer is not drawn, so nothing typed can leak into it. `Ctrl-C`
    /// still quits — a modal must not be able to trap the user — and the
    /// arrows scroll, because there is no composer cursor here to move.
    fn handle_viewer_key(&mut self, key: KeyEvent, esc_armed: bool) -> KeyResult {
        match key.code {
            KeyCode::Esc => return self.handle_esc(esc_armed),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return KeyResult::Action(Action::Quit);
            }
            KeyCode::Char('x') if key.modifiers.is_empty() => {
                if let Some(id) = self.focus {
                    self.app.stop_subagent(id);
                }
                return KeyResult::Handled;
            }
            KeyCode::Up | KeyCode::Down => {
                if let Some(view) = self.focus.and_then(|id| self.views.get_mut(&id)) {
                    view.scroll_lines(key.code == KeyCode::Up, VIEWER_SCROLL_LINES);
                }
                return KeyResult::Handled;
            }
            _ => {}
        }
        let result = match self.focus.and_then(|id| self.views.get_mut(&id)) {
            Some(view) => view.handle_key(key, &self.state),
            None => KeyResult::Ignored,
        };
        if let KeyResult::Action(Action::Notify(text)) = result {
            self.apply_event(&AppEvent::notification(text));
        }
        KeyResult::Handled
    }

    fn esc_armed(&self) -> bool {
        self.esc_confirm_until
            .is_some_and(|until| Instant::now() < until)
    }

    /// Drops an expired arm so the next Esc starts the confirm pair over.
    fn expire_esc_confirm(&mut self) {
        if self
            .esc_confirm_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.esc_confirm_until = None;
        }
    }

    /// Drops a pending arm, so nothing but a second Esc inside the window
    /// confirms the first one.
    fn clear_esc_confirm(&mut self) {
        self.esc_confirm_until = None;
    }

    /// The first press only arms the action the status bar announces; the
    /// second, inside the window, performs it.
    fn handle_esc(&mut self, armed: bool) -> KeyResult {
        let action = self.esc_action();
        if matches!(action, EscAction::Ignore) {
            return KeyResult::Handled;
        }
        if !armed && !action.acts_immediately() {
            self.esc_confirm_until = Some(Instant::now() + ESC_CONFIRM_WINDOW);
            return KeyResult::Handled;
        }
        match action {
            EscAction::CloseViewer => {
                self.focus = None;
                KeyResult::Handled
            }
            EscAction::PopQueue => {
                if let Some(text) = self.pending.pop() {
                    self.input.set_text(&text);
                }
                KeyResult::Handled
            }
            EscAction::Cancel => KeyResult::Action(Action::Cancel),
            EscAction::Rewind => {
                let Some(text) = self.transcript.rewind_text() else {
                    return KeyResult::Handled;
                };
                self.input.set_text(&text);
                self.rewinding = true;
                if self.app.rewind().is_err() {
                    self.rewinding = false;
                }
                KeyResult::Handled
            }
            EscAction::Ignore => KeyResult::Handled,
        }
    }

    fn esc_action(&self) -> EscAction {
        EscAction::new(
            self.pending.last().map(String::as_str),
            self.focus.is_some(),
            self.state.busy,
            self.rewinding,
            self.transcript.rewind_text().as_deref(),
        )
    }

    fn draw(&mut self, f: &mut Frame<'_>) {
        let area = f.area();
        let focused = self.focus.and_then(|id| self.views.get_mut(&id));
        let overlay_height = match self.prompt.as_ref() {
            Some(prompt) => prompt.height(area.width),
            None => self.input.overlay_height(),
        };
        let input_h = match self.focus {
            Some(_) => VIEWER_ROWS,
            None => self.input.height(area.width),
        };
        let regions = layout::split(
            area,
            input_h,
            queue::height(&self.pending),
            agents::height(&self.agents),
            self.todos.height(),
            overlay_height,
        );
        self.agents_area = regions.agents;

        match focused {
            Some(view) => view.draw(f, regions.transcript, &self.state),
            None => self.transcript.draw(f, regions.transcript, &self.state),
        }
        if let Some(queue) = regions.queue {
            queue::draw(f, queue, &self.pending);
        }
        if let Some(agents) = regions.agents {
            agents::draw(f, agents, &self.agents);
        }
        if let Some(todos) = regions.todos {
            self.todos.draw(f, todos);
        }
        if let Some(id) = self.focus {
            self.draw_viewer_hint(f, regions.input, id);
            self.status.draw_bar(f, regions.status, &self.state);
            return;
        }
        self.input.draw_composer(
            f,
            regions.input,
            &self.state,
            composer_hint(
                &self.input,
                self.state.busy,
                self.prompt.as_ref(),
                self.esc_armed(),
            ),
        );
        if let Some(overlay) = regions.overlay {
            match self.prompt.as_ref() {
                Some(prompt) => prompt.draw(f, overlay),
                None => self.input.draw_overlay(f, overlay),
            }
        }
        self.status.draw_bar(f, regions.status, &self.state);
        self.status.draw_reply_overlay(f, regions.transcript);
    }

    /// While a subagent's transcript owns the screen the composer has no
    /// work to do, so its row states what the viewer answers to instead.
    fn draw_viewer_hint(&self, f: &mut Frame<'_>, area: Rect, id: AgentId) {
        let Some(agent) = self.agents.iter().find(|agent| agent.id == id) else {
            return;
        };
        let text = format!(
            "{} · {} · {}",
            agent.name,
            agent.status.label(),
            hint::VIEWER
        );
        agents::draw_hint(f, area, &text);
    }

    fn wants_tick(&self) -> bool {
        self.state.working() || self.status.has_reply() || self.esc_armed()
    }
}

fn active_agents(agents: &[NodeInfo]) -> usize {
    agents
        .iter()
        .filter(|agent| agent.status.is_active())
        .count()
}

fn is_mode_toggle(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::BackTab)
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
}

/// The keys that apply right now, drawn on the composer border: whichever box
/// owns the keyboard states them, and the composer falls back to its own.
fn composer_hint(
    input: &InputView,
    busy: bool,
    prompt: Option<&OverlayPrompt>,
    esc_armed: bool,
) -> Option<&'static str> {
    let prompt = prompt.map(|prompt| {
        if prompt.awaits_typed_answer() {
            Prompt::Answer
        } else {
            Prompt::Keys(prompt.hint())
        }
    });
    hint::composer(input.overlay_hint(), prompt, busy, esc_armed)
}
fn unsent_notice(count: usize) -> String {
    let noun = match count {
        1 => "message",
        _ => "messages",
    };
    format!("dropped {count} queued {noun} (never sent)")
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
mod ui_test;

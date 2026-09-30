use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::EventStream;
use futures::StreamExt;
use oven_app::{AgentId, AnswerResponse, App, AppEvent, AppState, UserRequestId, UserResponse};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use tokio::sync::{mpsc, watch};

use crate::core::component::{Component, State};
use crate::widgets::input::{InputView, Overlay};
use crate::widgets::queue;
use crate::widgets::status::StatusBar;
use crate::widgets::todos::TodosWidget;
use crate::widgets::transcript::Transcript;

use crate::core::hint::{self, Prompt};
use crate::core::layout;
use crate::platform::terminal;

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
    /// Every subagent, its own transcript, and the one on screen. The driver's
    /// own conversation stays in `transcript`.
    views: Views,
    /// The driver. Anything else is a subagent, and its events belong to
    /// `views`.
    main_agent: AgentId,
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
            agents: Views::new().mirror(&snapshot.subagents),
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
            views: Views::new(),
            main_agent: snapshot.agent_id,
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
        self.state.agents = self.views.mirror(&subagents);
        self.maybe_flush();
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

    /// Stops what is still running before the process goes away: the turn
    /// first, then the subagents it may have spawned. `App::shutdown` cancels
    /// the supervisor as a backstop, but a subagent that is asked to stop is
    /// stopped in the registry too, which is what the last frame shows.
    fn shutdown_in_flight(&self) {
        if self.state.busy {
            self.send_cancel();
        }
        if !self.views.is_empty() {
            self.app.stop_subagents();
        }
    }

    pub(super) fn send_cancel(&self) {
        let turn_id = self.state_rx.borrow().phase.turn_id();
        if let Some(turn_id) = turn_id {
            self.app.cancel(turn_id);
        }
    }

    pub(super) fn respond_question(&self, request_id: UserRequestId, response: AnswerResponse) {
        self.app.respond(request_id, UserResponse::Answer(response));
    }

    fn esc_armed(&self) -> bool {
        self.esc_confirm_until
            .is_some_and(|until| Instant::now() < until)
    }

    fn draw(&mut self, f: &mut Frame<'_>) {
        let area = f.area();
        let overlay_height = match self.prompt.as_ref() {
            Some(prompt) => prompt.height(area.width),
            None => self.input.overlay_height(),
        };
        let input_h = match self.views.viewer_rows() {
            Some(rows) => rows,
            None => self.input.height(area.width),
        };
        let regions = layout::split(
            area,
            input_h,
            queue::height(&self.pending),
            self.views.height(),
            self.todos.height(),
            overlay_height,
        );
        self.agents_area = regions.agents;

        match self.views.focused() {
            Some(view) => view.draw(f, regions.transcript, &self.state),
            None => self.transcript.draw(f, regions.transcript, &self.state),
        }
        if let Some(queue) = regions.queue {
            queue::draw(f, queue, &self.pending);
        }
        if let Some(agents) = regions.agents {
            self.views.draw_strip(f, agents);
        }
        if let Some(todos) = regions.todos {
            self.todos.draw(f, todos);
        }
        if self.views.focused_id().is_some() {
            self.views.draw_hint(f, regions.input);
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

    fn wants_tick(&self) -> bool {
        self.state.working() || self.status.has_reply() || self.esc_armed()
    }
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

mod event;
mod keys;
mod prompt;
mod views;

use self::prompt::OverlayPrompt;
use self::views::Views;

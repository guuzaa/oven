use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::EventStream;
use futures::StreamExt;
use oven_app::{
    AnswerResponse, App, AppEvent, AppPhase, AppState, Command, CommandResult, Input,
    UserRequestId, UserResponse,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use tokio::sync::{mpsc, watch};

use crate::core::component::State;
use crate::widgets::input::InputView;
use crate::widgets::status::StatusBar;
use crate::widgets::todos::TodosWidget;
use crate::widgets::transcript::Transcript;

use crate::platform::terminal;

/// The screen's pace. A provider streams tens of chunks per second and each of
/// them used to repaint the whole transcript, which starved the input branch
/// until the wheel and `Ctrl-C` stalled; frames are coalesced onto this tick.
const FRAME_INTERVAL: Duration = Duration::from_millis(80);

pub struct Ui {
    app: App,
    events: mpsc::UnboundedReceiver<Arc<AppEvent>>,
    /// The app's levels — phase, mode, model, subagents — which the UI reads
    /// as they stand rather than replaying what changed.
    state_rx: watch::Receiver<AppState>,
    state: State,
    quit: bool,
    /// Esc is ignored until `Rewound` arrives so a second rewind cannot
    /// desync the transcript from the backend.
    rewinding: bool,
    /// The messages waiting to go to the app: the ones the user typed while
    /// the driver was busy.
    queue: Vec<Queued>,
    /// The requests waiting to go to the app. The key handler is
    /// synchronous, so a request waits here until the event loop sends it.
    outbox: Vec<Outbox>,
    /// Esc confirmed a rewind. `handle_key` stays sync and the flush sends it.
    pending_rewind: bool,
    /// Deadline for the Esc press that confirms a previous one.
    esc_confirm_until: Option<Instant>,

    transcript: Transcript,
    /// Every subagent, its own transcript, and the one on screen. The driver's
    /// own conversation stays in `transcript`. The driver's id lives here too.
    views: Views,
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
            InputView::new(app.slash_commands(), snapshot.levels.provider.clone()).with_root(&root);
        input.sync(&snapshot.levels);
        if snapshot.levels.configured_providers.is_empty() && snapshot.levels.provider.needs_setup()
        {
            input.open_setup();
        }
        let mut views = Views::new(snapshot.levels.agent_id);
        let state = State {
            busy: snapshot.levels.phase.is_active(),
            agents: views.mirror(&snapshot.levels.subagents),
            mode: snapshot.levels.mode,
            ..State::new()
        };
        Self {
            app,
            events,
            state_rx,
            state,
            quit: false,
            rewinding: false,
            queue: Vec::new(),
            outbox: Vec::new(),
            pending_rewind: false,
            esc_confirm_until: None,

            transcript: Transcript::new(),
            views,
            agents_area: None,
            status: StatusBar::new(
                snapshot.levels.model.clone(),
                &root,
                snapshot.levels.last_turn_usage,
            )
            .with_effort(snapshot.levels.reasoning_effort)
            .with_context(
                snapshot.levels.context_tokens,
                snapshot.levels.context_window,
            ),
            input,
            todos: TodosWidget::new(snapshot.levels.todos.clone()),
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
        let queued = std::mem::take(&mut self.queue);
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
        let mut tick = tokio::time::interval(FRAME_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        self.draw_frame(terminal)?;
        let mut dirty = false;
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    // The spinner, caret, and subagent clocks advance with no
                    // app event behind them. A tool call can sit silent for
                    // seconds, so the tick itself has to paint those frames.
                    let animated = self.state.working();
                    if animated {
                        self.state.frame = self.state.frame.wrapping_add(1);
                    }
                    let expired = self.status.expire_reply() || self.expire_esc_confirm();
                    if dirty || animated || expired {
                        dirty = false;
                        self.draw_frame(terminal)?;
                    }
                }
                Some(ev) = term_events.next() => {
                    let quit = self.handle_term_event(ev?)?;
                    // The requests the last key made go out before the app
                    // is shut down: a cancel or a stop must not be lost.
                    self.maybe_flush().await;
                    if quit {
                        break;
                    }
                    dirty = false;
                    self.draw_frame(terminal)?;
                }
                result = self.events.recv() => {
                    match result {
                        Some(ev) => self.apply_event(ev.as_ref()),
                        None => self.disconnected(),
                    }
                    self.drain_events();
                    if self.quit {
                        break;
                    }
                    dirty = true;
                }
                Ok(()) = self.state_rx.changed() => {
                    self.sync_state().await;
                    dirty = true;
                }
            }
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
                Ok(ev) => self.apply_event(ev.as_ref()),
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
    async fn sync_state(&mut self) {
        let subagents = {
            let state = self.state_rx.borrow_and_update();
            self.state.busy = state.levels.phase.is_active();
            self.state.mode = state.levels.mode;
            self.status.sync(&state.levels);
            self.input.sync(&state.levels);
            self.todos.sync(&state.levels);
            Arc::clone(&state.levels.subagents)
        };
        self.state.agents = self.views.mirror(&subagents);
        self.maybe_flush().await;
    }

    /// Sends what is waiting to go to the app.
    ///
    /// The requests go first: a chat parked with the running turn has to be
    /// parked before the queue is looked at, so the turn knows it may take
    /// it.
    async fn maybe_flush(&mut self) {
        if self.pending_rewind {
            self.pending_rewind = false;
            if self.app.apply(Command::Rewind).await.is_err() {
                self.rewinding = false;
            }
        }
        if !self.outbox.is_empty() {
            let requests = std::mem::take(&mut self.outbox);
            for request in requests {
                self.send(request).await;
            }
        }
        self.flush_queue().await;
    }

    /// Sends one request to the app.
    async fn send(&mut self, request: Outbox) {
        match request {
            Outbox::Command(command) => {
                let _ = self.app.apply(command).await;
            }
            Outbox::Steer { input } => {
                let parked = matches!(
                    self.app
                        .apply(Command::Steer {
                            input: input.clone()
                        })
                        .await,
                    Ok(CommandResult::Parked(_))
                );
                if parked && let Some(text) = chat_text(&input) {
                    self.mark_parked(text);
                }
            }
            Outbox::Unpark { text } => {
                if matches!(
                    self.app.apply(Command::Unpark { text: text.clone() }).await,
                    Ok(CommandResult::Unparked(_))
                ) {
                    self.input.set_text(&text);
                }
            }
        }
    }

    /// Sends the messages that are waiting for the driver.
    async fn flush_queue(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        if self.state.busy && self.queue.iter().all(|queued| !queued.is_immediate()) {
            return;
        }
        let queued = std::mem::take(&mut self.queue);
        let mut rest = Vec::new();
        let mut iter = queued.into_iter();
        while let Some(queued) = iter.next() {
            if self.state.busy && !queued.is_immediate() {
                rest.push(queued);
                continue;
            }
            let input = match self.take_back(&queued).await {
                Some(input) => input,
                None => continue,
            };
            match self.app.apply(Command::Submit { input }).await {
                Ok(CommandResult::Submitted(input)) => {
                    if queued.is_echoed() {
                        self.push_submitted(&input);
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    rest.push(queued);
                    rest.extend(iter);
                    break;
                }
            }
        }
        if !rest.is_empty() {
            rest.append(&mut self.queue);
            self.queue = rest;
        }
    }

    /// Takes a parked chat back out of the turn it was parked with, so it
    /// can go out as a message of its own.
    ///
    /// `None` when the turn already took it: the chat is in the
    /// conversation, and running it again would say it twice.
    async fn take_back(&self, queued: &Queued) -> Option<Input> {
        if !queued.is_parked() {
            return Some(queued.input.clone());
        }
        match self
            .app
            .apply(Command::Unpark {
                text: queued.text.clone(),
            })
            .await
        {
            Ok(CommandResult::Unparked(input)) => Some(input),
            _ => None,
        }
    }

    /// Marks the queued chat as parked with the running turn.
    fn mark_parked(&mut self, text: &str) {
        if let Some(queued) = self.queue.iter_mut().find(|queued| queued.text == text) {
            queued.dispatch = Dispatch::Parked;
        }
    }

    fn drop_appended(&mut self, text: &str) {
        if let Some(index) = self
            .queue
            .iter()
            .position(|queued| queued.is_parked() && queued.text == text)
        {
            self.queue.remove(index);
        }
    }

    /// Stops what is still running before the process goes away: the turn
    /// first, then the subagents it may have spawned. `App::shutdown` cancels
    /// the supervisor as a backstop, but a subagent that is asked to stop is
    /// stopped in the registry too, which is what the last frame shows.
    fn shutdown_in_flight(&mut self) {
        if self.state.busy {
            self.send_cancel();
        }
        if !self.views.is_empty() {
            self.outbox.push(Outbox::Command(Command::StopSubagents));
        }
    }

    pub(super) fn send_cancel(&mut self) {
        let phase = self.state_rx.borrow().levels.phase.clone();
        if let Some(turn_id) = phase.turn_id() {
            self.outbox
                .push(Outbox::Command(Command::Cancel { turn_id }));
            return;
        }
        if matches!(phase, AppPhase::Compacting) {
            self.outbox.push(Outbox::Command(Command::CancelCompaction));
        }
    }

    /// Answers the request a running turn is waiting on.
    fn respond(&mut self, request_id: UserRequestId, response: UserResponse) {
        self.outbox.push(Outbox::Command(Command::Respond {
            request_id,
            response,
        }));
    }

    pub(super) fn respond_question(&mut self, request_id: UserRequestId, response: AnswerResponse) {
        self.respond(request_id, UserResponse::Answer(response));
    }

    fn esc_armed(&self) -> bool {
        self.esc_confirm_until
            .is_some_and(|until| Instant::now() < until)
    }
}

/// A message waiting for its turn.
struct Queued {
    /// The text the user typed, drawn in the queue band.
    text: String,
    /// The classification of `text`. A message is classified once, when the
    /// user sends it, and goes out as it was classified.
    input: Input,
    /// When it goes out, and what happens to it.
    dispatch: Dispatch,
}

/// When a queued message goes out.
#[derive(Clone, Copy)]
enum Dispatch {
    /// Parked with the running turn, which appends it after the next tool
    /// results. A slash command or shell line stays local until the driver
    /// is free.
    Parked,
    /// Waiting for the driver: it goes out once the driver is free.
    Held,
    /// Sent on the next flush even while a turn is running (`/model`, `/setup`).
    Immediate {
        /// Drawn into the transcript when it goes out. A quiet submit is not.
        echo: bool,
    },
}

impl Queued {
    fn held(text: String, input: Input) -> Self {
        Self {
            text,
            input,
            dispatch: Dispatch::Held,
        }
    }

    fn now(text: String, input: Input, echo: bool) -> Self {
        Self {
            text,
            input,
            dispatch: Dispatch::Immediate { echo },
        }
    }

    /// Whether it goes out even while a turn is running.
    fn is_immediate(&self) -> bool {
        matches!(self.dispatch, Dispatch::Immediate { .. })
    }

    /// Whether the running turn may steer into it.
    fn is_parked(&self) -> bool {
        matches!(self.dispatch, Dispatch::Parked)
    }

    /// Whether it is drawn into the transcript when it goes out.
    fn is_echoed(&self) -> bool {
        match self.dispatch {
            Dispatch::Parked | Dispatch::Held => true,
            Dispatch::Immediate { echo } => echo,
        }
    }
}

/// A request waiting to go to the app.
///
/// The key handler is synchronous, so a request waits here until the event
/// loop sends it.
enum Outbox {
    /// A protocol command.
    Command(Command),
    /// A chat to park with the running turn.
    Steer { input: Input },
    /// A parked chat to take back out of the turn it was parked with, so it
    /// goes back into the composer.
    Unpark { text: String },
}

/// The text of an input, when it is a chat.
fn chat_text(input: &Input) -> Option<&str> {
    match input {
        Input::Chat(text) => Some(text),
        _ => None,
    }
}

fn unsent_notice(count: usize) -> String {
    let noun = match count {
        1 => "message",
        _ => "messages",
    };
    format!("dropped {count} queued {noun} (never sent)")
}

#[cfg(test)]
mod ui_test;

mod draw;
mod event;
mod keys;
mod prompt;
mod views;

use self::prompt::OverlayPrompt;
use self::views::Views;

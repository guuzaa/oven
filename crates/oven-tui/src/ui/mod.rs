mod component;
mod input;
mod model_picker;
mod queue;
mod setup_wizard;
mod slash_command_popup;
mod status;
mod theme;
mod transcript;

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use oven_app::{AppCmd, AppEvent, AppHandle};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use tokio::sync::mpsc;

use component::{Action, Component, KeyResult, State};
use input::{InputView, display_user_input};
use queue::QueueWidget;
use status::StatusBar;
use transcript::Transcript;

fn tool_display(name: &str, input: &serde_json::Value) -> String {
    if name == "bash"
        && let Some(command) = input.get("command").and_then(|v| v.as_str())
    {
        let command = command.trim();
        if !command.is_empty() {
            return command.to_string();
        }
    }
    name.to_string()
}

pub struct Ui {
    handle: AppHandle,
    events: mpsc::UnboundedReceiver<AppEvent>,
    state: State,
    quit: bool,
    /// A rewind is in flight: Esc is ignored until `Rewound` arrives so a
    /// second fallback cannot desync the transcript from the backend.
    rewinding: bool,
    transcript: Transcript,
    status: StatusBar,
    input: InputView,
    queue: QueueWidget,
    spin: u8,
}

impl Ui {
    pub fn new(handle: AppHandle) -> Self {
        let events = handle.subscribe();
        let slash_commands = handle.slash_commands().to_vec();
        let model = handle.model().to_string();
        let provider = handle.provider_config().clone();
        let root = handle
            .root()
            .canonicalize()
            .unwrap_or_else(|_| handle.root().to_owned());
        let total_usage = handle.total_usage();
        let mut input = InputView::new(slash_commands, provider.clone());
        if provider.needs_setup() {
            input.open_setup();
        }
        Self {
            handle,
            events,
            state: State::new(),
            quit: false,
            rewinding: false,
            transcript: Transcript::new(),
            status: StatusBar::new(model, &root, total_usage),
            input,
            queue: QueueWidget::new(),
            spin: 0,
        }
    }

    #[inline]
    fn load_transcript(&mut self) {
        self.transcript.seed(self.handle.history());
    }

    pub async fn run(mut self) -> io::Result<()> {
        self.load_transcript();
        let mut terminal = setup_terminal()?;
        let result = self.event_loop(&mut terminal).await;
        restore_terminal(&mut terminal)?;
        let session_id = self.handle.session_id();
        self.handle.shutdown().await;
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
        terminal.draw(|f| self.draw(f))?;
        loop {
            tokio::select! {
                _ = tick.tick(), if self.state.busy || self.status.has_reply() => {
                    if self.state.busy {
                        self.spin = self.spin.wrapping_add(1);
                    }
                    self.status.expire_reply();
                }
                Some(ev) = term_events.next() => {
                    match ev? {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            if self.handle_key(key) {
                                break;
                            }
                        }
                        Event::Paste(text) => {
                            self.input.paste(&text);
                        }
                        Event::Mouse(mouse) => {
                            self.handle_mouse(mouse);
                        }
                        Event::Resize(_, _) => {}
                        _ => continue,
                    }
                }
                result = self.events.recv() => {
                    match result {
                        Some(ev) => self.apply_event(ev),
                        None => {
                            self.state.busy = false;
                        }
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

    fn drain_events(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(ev) => self.apply_event(ev),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.state.busy = false;
                    break;
                }
            }
        }
    }

    fn apply_event(&mut self, ev: AppEvent) {
        if matches!(ev, AppEvent::Exit { .. }) {
            self.quit = true;
        }
        self.transcript.on_event(&ev, &mut self.state);
        self.status.on_event(&ev, &mut self.state);
        self.input.on_event(&ev, &mut self.state);
        if matches!(ev, AppEvent::Rewound { .. }) {
            self.rewinding = false;
        }
        self.maybe_flush();
    }

    /// Send each queued message to the app as its own turn once it is idle.
    fn maybe_flush(&mut self) {
        if self.state.busy || self.input.queue_len() == 0 {
            return;
        }
        let texts = self.input.drain_pending();
        self.state.busy = true;
        let remaining = send_each(texts, |text| {
            self.handle
                .send(AppCmd::UserInput(text.to_string()))
                .is_ok()
        });
        if !remaining.is_empty() {
            self.input.restore_pending(remaining);
            self.state.busy = false;
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        if let KeyResult::Action(Action::Notify(text)) =
            self.transcript.handle_mouse(mouse, &self.state)
        {
            self.apply_event(AppEvent::Notify {
                app_id: self.handle.id(),
                text,
            });
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        let result = match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                KeyResult::Action(Action::Quit)
            }
            KeyCode::Esc if !self.input.slash_open() => match EscAction::new(
                self.input.pop_pending(),
                self.state.busy,
                self.rewinding,
                self.transcript.last_user_text(),
            ) {
                EscAction::PopQueue(text) => {
                    self.transcript.pop_pending_user();
                    self.input.set_text(&text);
                    KeyResult::Handled
                }
                EscAction::Cancel => KeyResult::Action(Action::Cancel),
                EscAction::Rewind(text) => {
                    self.input.set_text(&text);
                    self.rewinding = true;
                    if self.handle.send(AppCmd::Rewind).is_err() {
                        self.rewinding = false;
                    }
                    KeyResult::Handled
                }
                EscAction::Ignore => KeyResult::Handled,
            },
            // While a rewind is in flight (idle, a few ms), plain Enter would
            // submit before the backend truncates history, letting the
            // Rewound event clobber the fresh submission. Block it until the
            // rewind completes; Alt-Enter newline still edits normally.
            KeyCode::Enter if self.rewinding && key.modifiers.is_empty() => KeyResult::Handled,
            _ => match self.transcript.handle_key(key, &self.state) {
                KeyResult::Ignored => self.input.handle_key(key, &self.state),
                other => other,
            },
        };

        match result {
            KeyResult::Ignored | KeyResult::Handled => false,
            KeyResult::Action(Action::Quit) => {
                if self.state.busy {
                    let _ = self.handle.send(AppCmd::Cancel);
                }
                true
            }
            KeyResult::Action(Action::Cancel) => {
                let _ = self.handle.send(AppCmd::Cancel);
                false
            }
            KeyResult::Action(Action::Queue(text)) => {
                self.transcript.push_user_queued(&display_user_input(&text));
                false
            }
            KeyResult::Action(Action::Submit(text)) => {
                self.transcript.push_user(&display_user_input(&text));
                self.status.clear_reply();
                self.input.clear();
                self.state.busy = true;
                if self.handle.send(AppCmd::UserInput(text)).is_err() {
                    self.state.busy = false;
                }
                false
            }
            KeyResult::Action(Action::QuietSubmit(text)) => {
                let _ = self.handle.send(AppCmd::UserInput(text));
                false
            }
            KeyResult::Action(Action::Notify(text)) => {
                self.apply_event(AppEvent::Notify {
                    app_id: self.handle.id(),
                    text,
                });
                false
            }
        }
    }

    fn draw(&mut self, f: &mut ratatui::Frame<'_>) {
        let avail = f.area().height;
        let reply_h = self
            .status
            .reply_height(f.area().width)
            .min(avail.saturating_sub(4));
        let chrome = 4 + reply_h;
        let input_h = self.input.height().min(avail.saturating_sub(chrome));
        let queue_h = self
            .queue
            .height(self.input.pending())
            .min(avail.saturating_sub(chrome + input_h));
        let setup_h = self.input.setup_height(&self.state);
        let picker_h = self.input.model_picker_height(&self.state);
        let slash_h = self.input.slash_command_height(&self.state);
        let popup_h = if setup_h > 0 {
            setup_h
        } else if picker_h > 0 {
            picker_h
        } else {
            slash_h
        }
        .min(avail.saturating_sub(chrome + input_h + queue_h));
        let mut constraints = vec![Constraint::Min(3)];
        if queue_h > 0 {
            constraints.push(Constraint::Length(queue_h));
        }
        constraints.push(Constraint::Length(input_h));
        if popup_h > 0 {
            constraints.push(Constraint::Length(popup_h));
        }
        constraints.push(Constraint::Length(1));
        if reply_h > 0 {
            constraints.push(Constraint::Length(reply_h));
        }
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(f.area());

        let mut next = 0;
        self.transcript.draw(f, chunks[next], &self.state);
        next += 1;
        if queue_h > 0 {
            self.queue.draw(f, chunks[next], self.input.pending());
            next += 1;
        }
        self.input.draw(f, chunks[next], &self.state);
        next += 1;
        if setup_h > 0 {
            self.input.draw_setup(f, chunks[next], &self.state);
            next += 1;
        } else if picker_h > 0 {
            self.input.draw_model_picker(f, chunks[next], &self.state);
            next += 1;
        } else if slash_h > 0 {
            self.input.draw_slash_command(f, chunks[next], &self.state);
            next += 1;
        }
        self.status.draw_bar(
            f,
            chunks[next],
            &self.state,
            self.input.slash_open(),
            self.spin,
        );
        if reply_h > 0 {
            next += 1;
            self.status.draw_reply(f, chunks[next]);
        }
    }
}

/// What pressing Esc should do, in priority order: pop a queued message,
/// interrupt an in-flight turn, or rewind the last finished exchange.
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

/// Send each message as its own `UserInput`, in order. Returns the messages
/// that were not sent: the first failed one and everything after it.
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

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_action_priority_queue_then_cancel_then_rewind() {
        // Queued message wins over everything else.
        assert!(matches!(
            EscAction::new(Some("q".into()), true, false, Some("u".into())),
            EscAction::PopQueue(t) if t == "q"
        ));
        assert!(matches!(
            EscAction::new(Some("q".into()), true, true, None),
            EscAction::PopQueue(t) if t == "q"
        ));
        // Empty queue + busy means interrupt, not fallback.
        assert!(matches!(
            EscAction::new(None, true, false, Some("u".into())),
            EscAction::Cancel
        ));
        // A rewind already in flight is ignored until it completes.
        assert!(matches!(
            EscAction::new(None, false, true, Some("u".into())),
            EscAction::Ignore
        ));
        // Idle with a previous user message rewinds it.
        assert!(matches!(
            EscAction::new(None, false, false, Some("u".into())),
            EscAction::Rewind(t) if t == "u"
        ));
        // Idle with nothing to fall back to is a no-op.
        assert!(matches!(
            EscAction::new(None, false, false, None),
            EscAction::Ignore
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
}

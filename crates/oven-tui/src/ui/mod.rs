mod component;
mod input;
mod model_picker;
mod queue;
mod slash_command_popup;
mod status;
mod transcript;

use std::io::{self, Stdout};

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use oven_app::{AgentEvent, AppCmd, AppEvent, AppHandle};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use tokio::sync::mpsc;

use component::{Action, Component, KeyResult, State};
use input::InputView;
use queue::QueueWidget;
use status::StatusBar;
use transcript::Transcript;

fn tool_display(name: &str, input: &serde_json::Value) -> String {
    if name == "bash"
        && let Some(command) = input.get("command").and_then(|v| v.as_str())
    {
        let command = command.trim();
        if !command.is_empty() {
            return format!("Ran {command}");
        }
    }
    name.to_string()
}

pub struct Ui {
    handle: AppHandle,
    events: mpsc::UnboundedReceiver<AppEvent>,
    state: State,
    quit: bool,
    transcript: Transcript,
    status: StatusBar,
    input: InputView,
    queue: QueueWidget,
}

impl Ui {
    pub fn new(handle: AppHandle) -> Self {
        let events = handle.subscribe();
        let slash_commands = handle.slash_commands().to_vec();
        let model = handle.model().to_string();
        let root = handle
            .root()
            .canonicalize()
            .unwrap_or_else(|_| handle.root().to_owned());
        Self {
            handle,
            events,
            state: State::new(),
            quit: false,
            transcript: Transcript::new(),
            status: StatusBar::new(model, &root),
            input: InputView::new(slash_commands),
            queue: QueueWidget::new(),
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
        self.handle.shutdown().await;
        result
    }

    async fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> io::Result<()> {
        let mut term_events = EventStream::new();
        terminal.draw(|f| self.draw(f))?;
        loop {
            tokio::select! {
                Some(ev) = term_events.next() => {
                    match ev? {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            if self.handle_key(key) {
                                break;
                            }
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
        if matches!(
            ev,
            AppEvent::Agent {
                event: AgentEvent::Exit { .. },
                ..
            }
        ) {
            self.quit = true;
        }
        self.transcript.on_event(&ev, &mut self.state);
        self.status.on_event(&ev, &mut self.state);
        self.input.on_event(&ev, &mut self.state);
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
        let _ = self.transcript.handle_mouse(mouse, &self.state);
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        let result = match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                KeyResult::Action(Action::Quit)
            }
            KeyCode::Esc if self.state.busy && !self.input.slash_open() => {
                KeyResult::Action(Action::Cancel)
            }
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
                self.transcript.push_user_queued(&text);
                false
            }
            KeyResult::Action(Action::Submit(text)) => {
                self.transcript.push_user(&text);
                self.input.clear();
                self.state.busy = true;
                if self.handle.send(AppCmd::UserInput(text)).is_err() {
                    self.state.busy = false;
                }
                false
            }
        }
    }

    fn draw(&mut self, f: &mut ratatui::Frame<'_>) {
        let input_h = self.input.height();
        let queue_h = self
            .queue
            .height(self.input.pending())
            .min(f.area().height.saturating_sub(3 + input_h));
        let mut constraints = vec![Constraint::Min(3)];
        if queue_h > 0 {
            constraints.push(Constraint::Length(queue_h));
        }
        constraints.push(Constraint::Length(input_h));
        let picker_h = self
            .input
            .model_picker_height(&self.state)
            .min(f.area().height.saturating_sub(3 + input_h + queue_h));
        let slash_command_h = self
            .input
            .slash_command_height(&self.state)
            .min(f.area().height.saturating_sub(3 + input_h + queue_h));
        let extra_h = if picker_h > 0 {
            picker_h
        } else {
            slash_command_h
        };
        if extra_h > 0 {
            constraints.push(Constraint::Length(extra_h));
        } else {
            constraints.push(Constraint::Length(1));
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
        if picker_h > 0 {
            self.input.draw_model_picker(f, chunks[next], &self.state);
        } else if slash_command_h > 0 {
            self.input.draw_slash_command(f, chunks[next], &self.state);
        } else {
            self.status.draw(f, chunks[next], &self.state);
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
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
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

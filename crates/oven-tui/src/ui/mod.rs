mod component;
mod input;
mod slash_command_popup;
mod status;
mod transcript;
mod usage;

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
use tokio::sync::broadcast;

use component::{Action, Component, KeyResult, State};
use input::InputView;
use status::StatusBar;
use transcript::Transcript;
use usage::UsageBar;

pub struct Ui {
    handle: AppHandle,
    events: broadcast::Receiver<AppEvent>,
    state: State,
    quit: bool,
    transcript: Transcript,
    status: StatusBar,
    usage: UsageBar,
    input: InputView,
}

impl Ui {
    pub fn new(handle: AppHandle) -> Self {
        let events = handle.subscribe();
        let slash_commands = handle.slash_commands().to_vec();
        Self {
            handle,
            events,
            state: State::new(),
            quit: false,
            transcript: Transcript::new(),
            status: StatusBar::new(),
            usage: UsageBar::new(),
            input: InputView::new(slash_commands),
        }
    }

    pub async fn run(mut self) -> io::Result<()> {
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
                        Ok(ev) => self.apply_event(ev),
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => {
                            self.status.set("app closed");
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
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(broadcast::error::TryRecvError::Closed) => {
                    self.status.set("app closed");
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
        self.usage.on_event(&ev, &mut self.state);
        self.input.on_event(&ev, &mut self.state);
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let _ = self.transcript.handle_mouse(mouse, &self.state);
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        let result = match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                KeyResult::Action(Action::Quit)
            }
            KeyCode::Esc if self.state.busy => KeyResult::Action(Action::Cancel),
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
                self.status.set("cancelling…");
                false
            }
            KeyResult::Action(Action::Submit(text)) => {
                self.transcript.push_user(&text);
                self.input.clear();
                self.state.busy = true;
                self.status.set("thinking…");
                if self.handle.send(AppCmd::UserInput(text)).is_err() {
                    self.state.busy = false;
                    self.status.set("app channel closed");
                }
                false
            }
        }
    }

    fn draw(&mut self, f: &mut ratatui::Frame<'_>) {
        let input_h = self.input.height();
        let slash_command_h = self
            .input
            .slash_command_height(&self.state)
            .min(f.area().height.saturating_sub(3 + 1 + 1 + input_h));
        let mut constraints = vec![
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(input_h),
        ];
        if slash_command_h > 0 {
            constraints.push(Constraint::Length(slash_command_h));
        }
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(f.area());

        self.transcript.draw(f, chunks[0], &self.state);
        self.status.draw(f, chunks[1], &self.state);
        self.usage.draw(f, chunks[2], &self.state);
        self.input.draw(f, chunks[3], &self.state);
        if slash_command_h > 0 {
            self.input.draw_slash_command(f, chunks[4], &self.state);
        }
    }
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

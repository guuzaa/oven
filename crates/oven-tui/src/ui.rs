use std::io;
use std::time::Duration;

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
};
use futures::StreamExt;
use oven_app::{AppCmd, AppEvent, AppHandle};
use tokio::sync::mpsc;

use crate::components::component::{Action, Component, KeyResult, State};
use crate::components::input::{InputView, Overlay, display_user_input};
use crate::components::queue::QueueWidget;
use crate::components::status::{StatusBar, StatusHint};
use crate::components::todos::TodosWidget;
use crate::components::transcript::Transcript;
use crate::components::{layout, terminal};

pub struct Ui {
    handle: AppHandle,
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
    queue: QueueWidget,
    todos: TodosWidget,
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
        let todos = handle.todos().clone();
        let mut input = InputView::new(slash_commands, provider.clone()).with_root(&root);
        if provider.needs_setup() {
            input.open_setup();
        }
        Self {
            handle,
            events,
            state: State::new(),
            quit: false,
            rewinding: false,
            pending: Vec::new(),
            transcript: Transcript::new(),
            status: StatusBar::new(model, &root, total_usage)
                .with_effort(provider.reasoning_effort),
            input,
            queue: QueueWidget::new(),
            todos: TodosWidget::new(todos),
        }
    }

    #[inline]
    fn load_transcript(&mut self) {
        self.transcript.seed(self.handle.history());
    }

    pub async fn run(mut self) -> io::Result<()> {
        self.load_transcript();
        let mut terminal = terminal::setup()?;
        let result = self.event_loop(&mut terminal).await;
        terminal::restore(&mut terminal)?;
        let session_id = self.handle.session_id();
        self.handle.shutdown().await;
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
        if matches!(ev, AppEvent::Idle { .. }) {
            self.state.busy = false;
        }
        if let AppEvent::ModeChanged { mode, .. } = &ev {
            self.state.mode = *mode;
        }
        self.transcript.on_event(&ev);
        self.status.on_event(&ev);
        self.input.on_event(&ev);
        self.todos.on_event(&ev);
        if matches!(ev, AppEvent::Rewound { .. }) {
            self.rewinding = false;
        }
        self.maybe_flush();
    }

    fn maybe_flush(&mut self) {
        if self.state.busy || self.pending.is_empty() {
            return;
        }
        let texts = std::mem::take(&mut self.pending);
        self.state.busy = true;
        let remaining = send_each(texts, |text| {
            if self
                .handle
                .send(AppCmd::UserInput(text.to_string()))
                .is_ok()
            {
                self.transcript.push_user(&display_user_input(text));
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
            _ if is_mode_toggle(key) => {
                self.state.mode = self.state.mode.toggle();
                let _ = self.handle.send(AppCmd::SetMode(self.state.mode));
                KeyResult::Handled
            }
            KeyCode::Esc if self.input.overlay() == Overlay::None => match EscAction::new(
                self.pending.pop(),
                self.state.busy,
                self.rewinding,
                self.transcript.last_user_text(),
            ) {
                EscAction::PopQueue(text) => {
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
            // Plain Enter during rewind would submit before history is truncated.
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
                self.pending.push(text);
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
        let area = f.area();
        let regions = layout::split(
            area,
            self.input.height(area.width),
            self.queue.height(&self.pending),
            self.todos.height(),
            self.input.overlay_height(),
            self.status.reply_height(area.width),
        );

        self.transcript.draw(f, regions.transcript, &self.state);
        if let Some(queue) = regions.queue {
            self.queue.draw(f, queue, &self.pending);
        }
        if let Some(todos) = regions.todos {
            self.todos.draw(f, todos);
        }
        self.input.draw(f, regions.input, &self.state);
        if let Some(overlay) = regions.overlay {
            self.input.draw_overlay(f, overlay);
        }
        self.status.draw_bar(
            f,
            regions.status,
            &self.state,
            status_hint(self.input.overlay(), self.state.busy),
        );
        if let Some(reply) = regions.reply {
            self.status.draw_reply(f, reply);
        }
    }

    fn wants_tick(&self) -> bool {
        self.state.busy || self.status.has_reply()
    }
}

fn is_mode_toggle(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::BackTab)
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
}

fn status_hint(overlay: Overlay, busy: bool) -> StatusHint {
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
        assert_eq!(status_hint(Overlay::Slash, true), StatusHint::Slash);
        assert_eq!(status_hint(Overlay::Setup, false), StatusHint::Modal);
        assert_eq!(status_hint(Overlay::Model, false), StatusHint::Modal);
        assert_eq!(status_hint(Overlay::None, true), StatusHint::Busy);
        assert_eq!(status_hint(Overlay::None, false), StatusHint::Idle);
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
}

use std::io;

use std::time::Instant;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use oven_app::AppEvent;
use ratatui::layout::Rect;

use crate::core::component::{Action, Component, KeyResult};
use crate::core::esc::{ESC_CONFIRM_WINDOW, EscAction};
use crate::core::keys::is_mode_toggle;
use crate::core::paste::{self, Burst};
use crate::widgets::input::Overlay;

use super::Ui;
use super::prompt::PromptFlow;

/// Lines an arrow key scrolls a subagent's transcript by.
const VIEWER_SCROLL_LINES: u16 = 1;

impl Ui {
    /// Returns `true` when the app should quit.
    pub(super) fn handle_term_event(&mut self, ev: Event) -> io::Result<bool> {
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

    fn handle_mouse(&mut self, mouse: MouseEvent, agents_area: Option<Rect>) {
        if let Some(area) = agents_area
            && let Some(id) = self.views.row_at(area, mouse.row)
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            self.views.focus(id);
            return;
        }
        // Whichever transcript is on screen takes the mouse: sending it to the
        // hidden one would scroll what nobody can see, and copy the wrong text
        // to the clipboard.
        let result = match self.views.focused() {
            Some(view) => view.handle_mouse(mouse, &self.state),
            None => self.transcript.handle_mouse(mouse, &self.state),
        };
        match result {
            KeyResult::Action(Action::Notify(text)) => {
                self.apply_event(&AppEvent::notification(text));
            }
            // The composer is not drawn while a viewer is open, so there is
            // nothing under the mouse there to hand the event to either.
            KeyResult::Ignored if self.views.focused_id().is_none() => {
                self.input.handle_mouse(mouse, &self.state);
            }
            _ => {}
        }
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) -> bool {
        let esc_armed = self.esc_armed();
        self.clear_esc_confirm();
        let result = match self.handle_prompt_key(key) {
            PromptFlow::Kept | PromptFlow::Closed => return false,
            PromptFlow::Typing => {
                let result = self.input.handle_key(key, &self.state);
                self.suppress_completions();
                result
            }
            PromptFlow::Free if self.views.focused_id().is_some() => {
                self.handle_viewer_key(key, esc_armed)
            }
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
                if let Some(id) = self.views.focused_id() {
                    self.app.stop_subagent(id);
                }
                return KeyResult::Handled;
            }
            KeyCode::Up | KeyCode::Down => {
                if let Some(view) = self.views.focused() {
                    view.scroll_lines(key.code == KeyCode::Up, VIEWER_SCROLL_LINES);
                }
                return KeyResult::Handled;
            }
            _ => {}
        }
        let result = match self.views.focused() {
            Some(view) => view.handle_key(key, &self.state),
            None => KeyResult::Ignored,
        };
        if let KeyResult::Action(Action::Notify(text)) = result {
            self.apply_event(&AppEvent::notification(text));
        }
        KeyResult::Handled
    }
    /// Drops an expired arm so the next Esc starts the confirm pair over.
    pub(super) fn expire_esc_confirm(&mut self) {
        if self
            .esc_confirm_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.esc_confirm_until = None;
        }
    }

    /// Drops a pending arm, so nothing but a second Esc inside the window
    /// confirms the first one.
    pub(super) fn clear_esc_confirm(&mut self) {
        self.esc_confirm_until = None;
    }

    /// The first press only arms the action the status bar announces; the
    /// second, inside the window, performs it.
    pub(super) fn handle_esc(&mut self, armed: bool) -> KeyResult {
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
                self.views.close();
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

    pub(super) fn esc_action(&self) -> EscAction {
        EscAction::new(
            self.pending.last().map(String::as_str),
            self.views.focused_id().is_some(),
            self.state.busy,
            self.rewinding,
            self.transcript.rewind_text().as_deref(),
        )
    }
}

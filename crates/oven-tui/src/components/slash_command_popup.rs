use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::list::{self, MAX_LIST_ROWS};
use super::theme;

/// Result of a key consumed by the slash-command popup.
pub(crate) enum SlashCommandPopupAction {
    /// The key was consumed without filling or submitting.
    Handled,
    /// Fill the input with the given command text.
    Fill(String),
    /// Submit the given input text.
    Submit(String),
}

/// Slash-command popup rendered below the input.
pub(crate) struct SlashCommandPopup {
    commands: Vec<(String, String)>,
    text: String,
    open: bool,
    selected: usize,
}

impl SlashCommandPopup {
    pub(crate) fn new(commands: Vec<(String, String)>) -> Self {
        Self {
            commands,
            text: String::new(),
            open: false,
            selected: 0,
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.selected = 0;
    }

    /// Re-evaluate visibility and selection from the input text.
    pub(crate) fn refresh(&mut self, text: &str) {
        let was_open = self.open;
        self.text = text.to_string();
        self.open = text.trim_start().starts_with('/');
        if !self.open {
            self.selected = 0;
            return;
        }
        if !was_open {
            self.selected = self
                .matches()
                .iter()
                .position(|&i| self.commands[i].0.to_lowercase() == self.token())
                .unwrap_or(0);
        }
        let n = self.matches().len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

    /// Index of the currently selected matching command, if any.
    pub(crate) fn selected_command(&self) -> Option<usize> {
        self.matches().get(self.selected).copied()
    }

    /// Indices of commands matching the current input token.
    pub(crate) fn matches(&self) -> Vec<usize> {
        let token = self.token();
        self.commands
            .iter()
            .enumerate()
            .filter(|(_, (name, _))| name.to_lowercase().starts_with(&token))
            .map(|(i, _)| i)
            .collect()
    }

    pub(crate) fn height(&self) -> u16 {
        if !self.open {
            return 0;
        }
        self.matches().len().clamp(1, MAX_LIST_ROWS) as u16
    }

    pub(crate) fn draw(&self, f: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let indices = self.matches();
        if indices.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled("no matching command", theme::dim())),
                area,
            );
            return;
        }
        list::draw_choice_list(
            f,
            area,
            indices.iter().take(MAX_LIST_ROWS).map(|&idx| {
                let (name, desc) = &self.commands[idx];
                (format!("/{name}"), desc.clone())
            }),
            self.selected,
        );
    }

    /// Handle keys while the popup is open.
    ///
    /// Returns `None` when the key should fall through to the input itself.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Option<SlashCommandPopupAction> {
        match key.code {
            KeyCode::Esc => {
                self.close();
                Some(SlashCommandPopupAction::Handled)
            }
            KeyCode::Up | KeyCode::Down => {
                let n = self.matches().len();
                list::cycle_selected(&mut self.selected, n, key.code == KeyCode::Up);
                Some(SlashCommandPopupAction::Handled)
            }
            KeyCode::Tab => {
                if let Some(idx) = self.selected_command() {
                    Some(SlashCommandPopupAction::Fill(self.fill_text(idx)))
                } else {
                    Some(SlashCommandPopupAction::Handled)
                }
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                if let Some(idx) = self.selected_command() {
                    if self.token() == self.commands[idx].0.to_lowercase() {
                        Some(SlashCommandPopupAction::Submit(
                            self.text.trim().to_string(),
                        ))
                    } else {
                        Some(SlashCommandPopupAction::Fill(self.fill_text(idx)))
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn token(&self) -> String {
        let trimmed = self.text.trim_start();
        if !trimmed.starts_with('/') {
            return String::new();
        }
        trimmed[1..]
            .split(char::is_whitespace)
            .next()
            .unwrap_or("")
            .to_lowercase()
    }

    fn fill_text(&self, idx: usize) -> String {
        format!("/{} ", self.commands[idx].0)
    }
}

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::component::State;

const MAX_COMMAND_ROWS: usize = 6;

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

    /// Height of the popup, or 0 when hidden.
    pub(crate) fn height(&self, _state: &State) -> u16 {
        if !self.open {
            return 0;
        }
        let rows = self.matches().len().clamp(1, MAX_COMMAND_ROWS);
        (rows as u16).saturating_add(2)
    }

    pub(crate) fn draw(&self, f: &mut Frame<'_>, area: Rect, _state: &State) {
        if !self.open {
            return;
        }
        let indices = self.matches();
        let mut lines = Vec::with_capacity(indices.len());
        for (row, &idx) in indices.iter().take(MAX_COMMAND_ROWS).enumerate() {
            let (name, desc) = &self.commands[idx];
            let style = if row == self.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(format!("/{name}"), style),
                Span::styled(format!("  {desc}"), style),
            ]));
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "no matching command",
                Style::default().fg(Color::DarkGray),
            )));
        }
        let block = Block::default().borders(Borders::ALL).title(" commands ");
        f.render_widget(Paragraph::new(lines).block(block), area);
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
            KeyCode::Up => {
                let n = self.matches().len();
                if n > 0 {
                    self.selected = (self.selected + n - 1) % n;
                }
                Some(SlashCommandPopupAction::Handled)
            }
            KeyCode::Down => {
                let n = self.matches().len();
                if n > 0 {
                    self.selected = (self.selected + 1) % n;
                }
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

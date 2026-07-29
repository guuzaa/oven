use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders};
use tui_textarea::TextArea;

use super::component::{Action, Component, KeyResult, State};

pub struct InputView {
    textarea: TextArea<'static>,
}

impl InputView {
    pub fn new() -> Self {
        Self {
            textarea: new_textarea(),
        }
    }

    pub fn height(&self) -> u16 {
        (self.textarea.lines().len() as u16)
            .clamp(1, 8)
            .saturating_add(2)
    }

    pub fn clear(&mut self) {
        self.textarea = new_textarea();
    }
}

impl Component for InputView {
    fn handle_key(&mut self, key: KeyEvent, state: &State) -> KeyResult {
        if state.busy {
            return KeyResult::Ignored;
        }
        match key.code {
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.textarea.insert_newline();
                KeyResult::Handled
            }
            KeyCode::Enter => {
                let text = self.textarea.lines().join("\n");
                let text = text.trim().to_string();
                if text.is_empty() {
                    KeyResult::Handled
                } else {
                    KeyResult::Action(Action::Submit(text))
                }
            }
            _ => {
                self.textarea.input(key);
                KeyResult::Handled
            }
        }
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect, state: &State) {
        let title = if state.busy {
            " input (busy) "
        } else {
            " input "
        };
        self.textarea
            .set_block(Block::default().borders(Borders::ALL).title(title));
        if state.busy {
            self.textarea
                .set_style(Style::default().fg(ratatui::style::Color::DarkGray));
            self.textarea.set_cursor_style(Style::default());
            self.textarea.set_cursor_line_style(Style::default());
        } else {
            self.textarea.set_style(Style::default());
            self.textarea
                .set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
            self.textarea.set_cursor_line_style(Style::default());
        }
        f.render_widget(&self.textarea, area);
    }
}

fn new_textarea() -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_cursor_line_style(Style::default());
    ta.set_placeholder_text("message…");
    ta
}

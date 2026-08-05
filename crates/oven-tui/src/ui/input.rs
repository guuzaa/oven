use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use tui_textarea::TextArea;
use unicode_width::UnicodeWidthStr;

use super::component::{Action, Component, KeyResult, State};

const MAX_MENU_ROWS: usize = 6;

pub struct InputView {
    textarea: TextArea<'static>,
    commands: Vec<(String, String)>,
    menu_open: bool,
    selected: usize,
}

impl InputView {
    pub fn new(commands: Vec<(String, String)>) -> Self {
        Self {
            textarea: new_textarea(),
            commands,
            menu_open: false,
            selected: 0,
        }
    }

    pub fn height(&self) -> u16 {
        (self.textarea.lines().len() as u16)
            .clamp(1, 8)
            .saturating_add(2)
    }

    pub fn clear(&mut self) {
        self.textarea = new_textarea();
        self.menu_open = false;
        self.selected = 0;
    }

    /// Height of the command popup below the input, or 0 when hidden.
    pub fn menu_height(&self, state: &State) -> u16 {
        if state.busy || !self.menu_open {
            return 0;
        }
        let rows = self.matches().len().clamp(1, MAX_MENU_ROWS);
        (rows as u16).saturating_add(2)
    }

    pub fn draw_menu(&mut self, f: &mut Frame<'_>, area: Rect, state: &State) {
        if state.busy || !self.menu_open {
            return;
        }
        let indices = self.matches();
        let mut lines = Vec::with_capacity(indices.len());
        for (row, &idx) in indices.iter().take(MAX_MENU_ROWS).enumerate() {
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

    fn text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    fn token(&self) -> String {
        let text = self.text();
        let trimmed = text.trim_start();
        if !trimmed.starts_with('/') {
            return String::new();
        }
        trimmed[1..]
            .split(char::is_whitespace)
            .next()
            .unwrap_or("")
            .to_lowercase()
    }

    fn matches(&self) -> Vec<usize> {
        let token = self.token();
        self.commands
            .iter()
            .enumerate()
            .filter(|(_, (name, _))| name.to_lowercase().starts_with(&token))
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_command(&self) -> Option<usize> {
        self.matches().get(self.selected).copied()
    }

    fn refresh_menu(&mut self) {
        let was_open = self.menu_open;
        self.menu_open = self.text().trim_start().starts_with('/');
        if !self.menu_open {
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

    fn fill_command(&mut self, idx: usize) {
        let text = format!("/{} ", self.commands[idx].0);
        let col = text.width();
        self.textarea.set_lines(vec![text], (0, col));
        self.refresh_menu();
    }
}

impl Component for InputView {
    fn handle_key(&mut self, key: KeyEvent, state: &State) -> KeyResult {
        if state.busy {
            return KeyResult::Ignored;
        }
        self.refresh_menu();
        if self.menu_open {
            match key.code {
                KeyCode::Esc => {
                    self.menu_open = false;
                    self.selected = 0;
                    return KeyResult::Handled;
                }
                KeyCode::Up => {
                    let n = self.matches().len();
                    if n > 0 {
                        self.selected = (self.selected + n - 1) % n;
                    }
                    return KeyResult::Handled;
                }
                KeyCode::Down => {
                    let n = self.matches().len();
                    if n > 0 {
                        self.selected = (self.selected + 1) % n;
                    }
                    return KeyResult::Handled;
                }
                KeyCode::Tab => {
                    if let Some(idx) = self.selected_command() {
                        self.fill_command(idx);
                    }
                    return KeyResult::Handled;
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    if let Some(idx) = self.selected_command() {
                        if self.token() == self.commands[idx].0.to_lowercase() {
                            let submitted = self.text().trim().to_string();
                            self.clear();
                            return KeyResult::Action(Action::Submit(submitted));
                        }
                        self.fill_command(idx);
                        return KeyResult::Handled;
                    }
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.textarea.insert_newline();
                KeyResult::Handled
            }
            KeyCode::Enter => {
                let text = self.text().trim().to_string();
                if text.is_empty() {
                    KeyResult::Handled
                } else {
                    self.clear();
                    KeyResult::Action(Action::Submit(text))
                }
            }
            _ => {
                self.textarea.input(key);
                self.refresh_menu();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn commands() -> Vec<(String, String)> {
        vec![
            ("clear".into(), "Clear conversation history.".into()),
            ("exit".into(), "End the session.".into()),
        ]
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(view: &mut InputView, text: &str) {
        for ch in text.chars() {
            view.handle_key(key(KeyCode::Char(ch)), &State::new());
        }
    }

    #[test]
    fn menu_opens_on_slash() {
        let mut view = InputView::new(commands());
        type_text(&mut view, "/");
        assert!(view.menu_open);
        assert_eq!(view.menu_height(&State::new()), 4);
    }

    #[test]
    fn menu_filters_by_prefix() {
        let mut view = InputView::new(commands());
        type_text(&mut view, "/c");
        assert_eq!(view.matches(), vec![0]);
    }

    #[test]
    fn menu_filter_is_case_insensitive() {
        let mut view = InputView::new(commands());
        type_text(&mut view, "/E");
        assert_eq!(view.matches(), vec![1]);
    }

    #[test]
    fn menu_closes_when_slash_deleted() {
        let mut view = InputView::new(commands());
        type_text(&mut view, "/c");
        view.handle_key(key(KeyCode::Backspace), &State::new());
        view.handle_key(key(KeyCode::Backspace), &State::new());
        assert!(!view.menu_open);
    }

    #[test]
    fn no_match_yields_empty_list() {
        let mut view = InputView::new(commands());
        type_text(&mut view, "/nope");
        assert!(view.matches().is_empty());
    }

    #[test]
    fn esc_closes_menu() {
        let mut view = InputView::new(commands());
        type_text(&mut view, "/");
        let result = view.handle_key(key(KeyCode::Esc), &State::new());
        assert!(matches!(result, KeyResult::Handled));
        assert!(!view.menu_open);
    }

    #[test]
    fn enter_exact_match_submits() {
        let mut view = InputView::new(commands());
        type_text(&mut view, "/exit");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::Submit(text)) => assert_eq!(text, "/exit"),
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn enter_with_args_preserves_them() {
        let mut view = InputView::new(commands());
        type_text(&mut view, "/clear hi");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::Submit(text)) => assert_eq!(text, "/clear hi"),
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn enter_prefix_fills_command() {
        let mut view = InputView::new(commands());
        type_text(&mut view, "/ex");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        assert!(matches!(result, KeyResult::Handled));
        assert_eq!(view.textarea.lines()[0], "/exit ");
        assert!(view.menu_open);
    }

    #[test]
    fn tab_fills_selected() {
        let mut view = InputView::new(commands());
        type_text(&mut view, "/c");
        let result = view.handle_key(key(KeyCode::Tab), &State::new());
        assert!(matches!(result, KeyResult::Handled));
        assert_eq!(view.textarea.lines()[0], "/clear ");
    }

    #[test]
    fn arrows_change_selection() {
        let mut view = InputView::new(commands());
        type_text(&mut view, "/");
        assert_eq!(view.selected, 0);
        view.handle_key(key(KeyCode::Down), &State::new());
        assert_eq!(view.selected, 1);
        view.handle_key(key(KeyCode::Down), &State::new());
        assert_eq!(view.selected, 0);
    }

    #[test]
    fn enter_without_match_submits_typed_text() {
        let mut view = InputView::new(commands());
        type_text(&mut view, "/nope");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::Submit(text)) => assert_eq!(text, "/nope"),
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn busy_ignores_keys() {
        let mut view = InputView::new(commands());
        type_text(&mut view, "/");
        assert!(view.menu_open);
        let state = State { busy: true };
        let result = view.handle_key(key(KeyCode::Char('x')), &state);
        assert!(matches!(result, KeyResult::Ignored));
    }
}

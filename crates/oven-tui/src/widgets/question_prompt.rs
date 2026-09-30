use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::{Paragraph, Wrap};

use oven_app::QuestionOption;

use super::list;
use super::transcript::wrap;
use crate::core::theme;

const TITLE: &str = "the agent is asking";
const OTHER_LABEL: &str = "Other…";
const OTHER_DESCRIPTION: &str = "type your own answer";
const TITLE_ROWS: u16 = 1;

pub(crate) enum QuestionPromptAction {
    /// The prompt consumed the key.
    Handled,
    /// The key belongs to the input box: the user is typing an answer.
    Typing,
    /// The user picked this answer.
    Answered(String),
    /// The user skipped the question.
    Declined,
    /// The user cancelled the whole turn.
    Cancelled,
}

/// A question from the agent, answered either by picking one of the offered
/// options or by typing a reply into the input box.
pub(crate) struct QuestionPrompt {
    question: String,
    options: Vec<QuestionOption>,
    selected: usize,
    typing: bool,
}

impl QuestionPrompt {
    pub(crate) const HINT: &str = "enter answer · esc skip · ctrl-c cancel";

    pub(crate) fn new(question: String, options: Vec<QuestionOption>) -> Self {
        Self {
            question,
            options,
            selected: 0,
            typing: false,
        }
    }

    /// Whether the prompt is waiting for text from the input box.
    pub(crate) fn awaits_typed_answer(&self) -> bool {
        self.typing
    }

    fn rows(&self) -> usize {
        self.options.len() + 1
    }

    fn items(&self) -> impl Iterator<Item = (&str, &str)> {
        self.options
            .iter()
            .map(|option| {
                (
                    option.label.as_str(),
                    option.description.as_deref().unwrap_or_default(),
                )
            })
            .chain(std::iter::once((OTHER_LABEL, OTHER_DESCRIPTION)))
    }

    pub(crate) fn height(&self, width: u16) -> u16 {
        TITLE_ROWS + self.question_rows(width) + u16::try_from(self.rows()).unwrap_or(u16::MAX)
    }

    /// Rows the question occupies once wrapped to `width`: the prompt grows
    /// with it instead of clipping, so the user reads all of what was asked.
    fn question_rows(&self, width: u16) -> u16 {
        let width = usize::from(width).max(1);
        let rows: usize = self
            .question
            .lines()
            .map(|line| {
                let mut rows = 0;
                let mut rest = line;
                loop {
                    let (_, tail) = wrap::split_at_width(rest, width);
                    rows += 1;
                    if tail.is_empty() {
                        break;
                    }
                    rest = tail;
                }
                rows
            })
            .sum();
        u16::try_from(rows.max(1)).unwrap_or(u16::MAX)
    }

    pub(crate) fn draw(&self, f: &mut Frame<'_>, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(TITLE_ROWS),
                Constraint::Length(self.question_rows(area.width)),
                Constraint::Min(0),
            ])
            .split(area);
        f.render_widget(Paragraph::new(TITLE).style(theme::accent()), rows[0]);
        f.render_widget(
            Paragraph::new(self.question.as_str()).wrap(Wrap { trim: true }),
            rows[1],
        );
        let selected = if self.typing {
            self.options.len()
        } else {
            self.selected
        };
        // `items` always ends in the "Other…" row, so no empty hint is reachable.
        list::draw_choice_list(f, rows[2], "", self.items(), selected);
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> QuestionPromptAction {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return QuestionPromptAction::Cancelled;
        }
        if self.typing {
            return match key.code {
                KeyCode::Esc => {
                    self.typing = false;
                    QuestionPromptAction::Handled
                }
                _ => QuestionPromptAction::Typing,
            };
        }
        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => match self.options.get(self.selected) {
                Some(option) => QuestionPromptAction::Answered(option.label.clone()),
                // The "Other…" row: the input box takes over from here.
                None => {
                    self.typing = true;
                    QuestionPromptAction::Handled
                }
            },
            KeyCode::Esc => QuestionPromptAction::Declined,
            _ => {
                let rows = self.rows();
                list::cycle_key(key, &mut self.selected, rows);
                QuestionPromptAction::Handled
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    const WIDTH: u16 = 40;
    const NARROW: u16 = 20;
    const WIDE: u16 = 120;
    const WORD_COUNT: usize = 30;

    fn option(label: &str) -> QuestionOption {
        QuestionOption {
            label: label.to_string(),
            description: None,
        }
    }

    fn with_options(question: &str) -> QuestionPrompt {
        QuestionPrompt::new(
            question.to_string(),
            vec![option("postgres"), option("sqlite")],
        )
    }

    fn without_options(question: &str) -> QuestionPrompt {
        QuestionPrompt::new(question.to_string(), Vec::new())
    }

    fn long_question() -> String {
        "context ".repeat(WORD_COUNT)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The prompt drawn at `WIDTH`, one string per rendered row.
    fn drawn(prompt: &QuestionPrompt) -> Vec<String> {
        let height = prompt.height(WIDTH);
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, height)).unwrap();
        terminal.draw(|f| prompt.draw(f, f.area())).unwrap();
        let buf = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..WIDTH)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn enter_picks_the_selected_option() {
        let mut prompt = with_options("which database?");
        let action = prompt.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, QuestionPromptAction::Answered(label) if label == "postgres"));
    }

    #[test]
    fn arrows_walk_every_row_including_other() {
        let mut prompt = with_options("which database?");
        prompt.handle_key(key(KeyCode::Down));
        let action = prompt.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, QuestionPromptAction::Answered(label) if label == "sqlite"));
    }

    #[test]
    fn choosing_other_hands_over_to_the_input_box() {
        let mut prompt = with_options("which database?");
        prompt.handle_key(key(KeyCode::Down));
        prompt.handle_key(key(KeyCode::Down));
        assert!(!prompt.awaits_typed_answer());
        assert!(matches!(
            prompt.handle_key(key(KeyCode::Enter)),
            QuestionPromptAction::Handled
        ));
        assert!(prompt.awaits_typed_answer());
        assert!(matches!(
            prompt.handle_key(key(KeyCode::Char('d'))),
            QuestionPromptAction::Typing
        ));
    }

    #[test]
    fn escape_returns_from_typing_to_the_options() {
        let mut prompt = with_options("which database?");
        prompt.handle_key(key(KeyCode::Down));
        prompt.handle_key(key(KeyCode::Down));
        prompt.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            prompt.handle_key(key(KeyCode::Esc)),
            QuestionPromptAction::Handled
        ));
        assert!(!prompt.awaits_typed_answer());
    }

    #[test]
    fn escape_skips_the_question() {
        let mut prompt = with_options("which database?");
        assert!(matches!(
            prompt.handle_key(key(KeyCode::Esc)),
            QuestionPromptAction::Declined
        ));
    }

    #[test]
    fn ctrl_c_cancels_the_turn_in_either_mode() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut prompt = with_options("which database?");
        assert!(matches!(
            prompt.handle_key(ctrl_c),
            QuestionPromptAction::Cancelled
        ));
        let mut prompt = with_options("which database?");
        prompt.handle_key(key(KeyCode::Down));
        prompt.handle_key(key(KeyCode::Down));
        prompt.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            prompt.handle_key(ctrl_c),
            QuestionPromptAction::Cancelled
        ));
    }

    #[test]
    fn a_question_without_options_starts_with_other_selected() {
        let mut prompt = without_options("what should I name it?");
        assert!(matches!(
            prompt.handle_key(key(KeyCode::Enter)),
            QuestionPromptAction::Handled
        ));
        assert!(prompt.awaits_typed_answer());
    }

    #[test]
    fn height_covers_the_question_and_every_row() {
        assert_eq!(
            with_options("which database?").height(WIDTH),
            TITLE_ROWS + 4
        );
        assert_eq!(without_options("q").height(WIDTH), TITLE_ROWS + 2);
    }

    #[test]
    fn a_long_question_is_given_the_rows_it_wraps_into() {
        let long = without_options(&long_question());
        assert_eq!(without_options("short").question_rows(WIDE), 1);
        assert!(long.question_rows(WIDE) > 1);
        assert!(long.question_rows(NARROW) > long.question_rows(WIDE));
        assert!(long.height(NARROW) > long.height(WIDE));
    }

    #[test]
    fn the_whole_question_is_drawn() {
        let long = without_options(&long_question());
        let rows = drawn(&long);
        let question: String = rows[1..=usize::from(long.question_rows(WIDTH))].join("");
        assert_eq!(
            question.split_whitespace().collect::<Vec<_>>(),
            long_question().split_whitespace().collect::<Vec<_>>(),
            "every word of the question must be visible"
        );
        assert!(
            rows.last().unwrap().contains(OTHER_LABEL),
            "{:?}",
            rows.last()
        );
    }

    #[test]
    fn a_multi_line_question_keeps_its_lines() {
        let prompt = without_options("first line\nsecond line");
        let rows = drawn(&prompt);
        assert!(rows[1].contains("first line"), "{:?}", rows[1]);
        assert!(rows[2].contains("second line"), "{:?}", rows[2]);
    }

    #[test]
    fn selected_row_is_visible_in_the_options_block() {
        let mut prompt = with_options("which database?");
        prompt.handle_key(key(KeyCode::Down));
        prompt.handle_key(key(KeyCode::Down));
        let rows = drawn(&prompt);
        let option_row = usize::from(TITLE_ROWS + prompt.question_rows(WIDTH));
        assert!(rows[0].contains(TITLE), "{:?}", rows[0]);
        assert!(rows[1].contains("which database?"), "{:?}", rows[1]);
        assert!(
            rows[option_row].contains("postgres"),
            "{:?}",
            rows[option_row]
        );
        assert!(!rows[option_row].contains(list::SELECTED_MARK));
        let other_row = option_row + 2;
        assert!(
            rows[other_row].contains(&format!("{}{OTHER_LABEL}", list::SELECTED_MARK)),
            "{:?}",
            rows[other_row]
        );
    }
}

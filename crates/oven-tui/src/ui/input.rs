use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use oven_app::AppEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders};
use tui_textarea::TextArea;
use unicode_width::UnicodeWidthStr;

use super::component::{Action, Component, KeyResult, State};
use super::model_picker::{ModelPicker, ModelPickerAction};
use super::slash_command_popup::{SlashCommandPopup, SlashCommandPopupAction};

pub struct InputView {
    textarea: TextArea<'static>,
    slash_command: SlashCommandPopup,
    model_picker: ModelPicker,
    /// Messages accepted while the app is busy, flushed once it idles again.
    pending: Vec<String>,
}

impl InputView {
    pub fn new(commands: Vec<(String, String)>) -> Self {
        Self {
            textarea: new_textarea(),
            slash_command: SlashCommandPopup::new(commands),
            model_picker: ModelPicker::new(Vec::new()),
            pending: Vec::new(),
        }
    }

    pub fn height(&self) -> u16 {
        (self.textarea.lines().len() as u16)
            .clamp(1, 8)
            .saturating_add(2)
    }

    pub fn clear(&mut self) {
        self.textarea = new_textarea();
        self.slash_command.close();
        self.model_picker.close();
    }

    /// Height of the command popup below the input, or 0 when hidden.
    pub fn slash_command_height(&self, state: &State) -> u16 {
        self.slash_command.height(state)
    }

    pub fn draw_slash_command(&mut self, f: &mut Frame<'_>, area: Rect, state: &State) {
        self.slash_command.draw(f, area, state);
    }

    /// Height of the model picker below the input, or 0 when hidden.
    pub fn model_picker_height(&self, state: &State) -> u16 {
        self.model_picker.height(state)
    }

    pub fn draw_model_picker(&mut self, f: &mut Frame<'_>, area: Rect, state: &State) {
        self.model_picker.draw(f, area, state);
    }

    /// Number of messages waiting to be flushed to the app.
    pub(crate) fn queue_len(&self) -> usize {
        self.pending.len()
    }

    /// Messages waiting to be flushed, in queue order.
    pub(crate) fn pending(&self) -> &[String] {
        &self.pending
    }

    /// Take all queued messages in order, clearing the queue.
    pub(crate) fn drain_pending(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending)
    }

    /// Put messages back at the front of the queue (e.g. after a failed send).
    pub(crate) fn restore_pending(&mut self, mut texts: Vec<String>) {
        texts.append(&mut self.pending);
        self.pending = texts;
    }

    /// Whether the slash-command popup or the model picker is currently open.
    pub(crate) fn slash_open(&self) -> bool {
        self.slash_command.is_open() || self.model_picker.is_open()
    }

    fn text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    fn fill_command(&mut self, text: &str) {
        let col = text.width();
        self.textarea.set_lines(vec![text.to_string()], (0, col));
        self.slash_command.refresh(&self.text());
    }
}

impl Component for InputView {
    fn on_event(&mut self, ev: &AppEvent, _state: &mut State) {
        if let AppEvent::ModelsUpdated { models, .. } = ev {
            self.model_picker.update_models(models.clone());
        }
    }

    fn handle_key(&mut self, key: KeyEvent, state: &State) -> KeyResult {
        let text = self.text();
        self.slash_command.refresh(&text);

        if self.model_picker.is_open() {
            return match self.model_picker.handle_key(key) {
                ModelPickerAction::Handled => KeyResult::Handled,
                ModelPickerAction::Submit(text) => {
                    self.clear();
                    KeyResult::Action(Action::Submit(text))
                }
                ModelPickerAction::Close => {
                    self.fill_command("/model");
                    KeyResult::Handled
                }
            };
        }

        if self.slash_command.is_open()
            && let Some(action) = self.slash_command.handle_key(key)
        {
            return match action {
                SlashCommandPopupAction::Handled => KeyResult::Handled,
                SlashCommandPopupAction::Fill(text) => {
                    self.fill_command(&text);
                    KeyResult::Handled
                }
                SlashCommandPopupAction::Submit(text) => {
                    // `/model` (with at most one fragment) opens the picker
                    // instead of submitting; two or more args keep the manual
                    // fast path.
                    if let Some(filter) = model_filter_from(&text) {
                        self.model_picker.open(&filter);
                        self.fill_command("/model ");
                        return KeyResult::Handled;
                    }
                    self.clear();
                    KeyResult::Action(Action::Submit(text))
                }
            };
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
                } else if state.busy {
                    self.pending.push(text.clone());
                    self.clear();
                    KeyResult::Action(Action::Queue(text))
                } else {
                    self.clear();
                    KeyResult::Action(Action::Submit(text))
                }
            }
            _ => {
                self.textarea.input(key);
                self.slash_command.refresh(&self.text());
                KeyResult::Handled
            }
        }
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect, state: &State) {
        let title = if state.busy {
            " input (busy) ".to_string()
        } else {
            " input ".to_string()
        };
        self.textarea
            .set_block(Block::default().borders(Borders::ALL).title(title));
        self.textarea.set_style(Style::default());
        self.textarea
            .set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        self.textarea.set_cursor_line_style(Style::default());
        f.render_widget(&self.textarea, area);
    }
}

/// If `text` is a `/model` command with at most one argument (`/model` or
/// `/model <fragment>`), return the fragment to seed the picker filter with.
/// Two or more arguments return `None` so the line submits directly.
fn model_filter_from(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let body = trimmed.strip_prefix('/')?;
    let mut words = body.split_whitespace();
    if !words.next()?.eq_ignore_ascii_case("model") {
        return None;
    }
    let fragment = words.next().unwrap_or("");
    if words.next().is_some() {
        return None;
    }
    Some(fragment.to_string())
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
            ("model".into(), "Switch model and reasoning effort.".into()),
        ]
    }

    fn models() -> Vec<(String, String)> {
        vec![
            ("gpt-4o".into(), "OpenAI".into()),
            ("gpt-4o-mini".into(), "OpenAI".into()),
            ("deepseek-chat".into(), "DeepSeek".into()),
        ]
    }

    fn view() -> InputView {
        let mut view = InputView::new(commands());
        view.model_picker.update_models(models());
        view
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
    fn slash_command_opens_on_slash() {
        let mut view = view();
        type_text(&mut view, "/");
        assert!(view.slash_command.is_open());
        assert_eq!(view.slash_command_height(&State::new()), 5);
    }

    #[test]
    fn slash_command_filters_by_prefix() {
        let mut view = view();
        type_text(&mut view, "/c");
        assert_eq!(view.slash_command.matches(), vec![0]);
    }

    #[test]
    fn slash_command_filter_is_case_insensitive() {
        let mut view = view();
        type_text(&mut view, "/E");
        assert_eq!(view.slash_command.matches(), vec![1]);
    }

    #[test]
    fn slash_command_closes_when_slash_deleted() {
        let mut view = view();
        type_text(&mut view, "/c");
        view.handle_key(key(KeyCode::Backspace), &State::new());
        view.handle_key(key(KeyCode::Backspace), &State::new());
        assert!(!view.slash_command.is_open());
    }

    #[test]
    fn no_match_yields_empty_list() {
        let mut view = view();
        type_text(&mut view, "/nope");
        assert!(view.slash_command.matches().is_empty());
    }

    #[test]
    fn esc_closes_slash_command() {
        let mut view = view();
        type_text(&mut view, "/");
        let result = view.handle_key(key(KeyCode::Esc), &State::new());
        assert!(matches!(result, KeyResult::Handled));
        assert!(!view.slash_command.is_open());
    }

    #[test]
    fn enter_exact_match_submits() {
        let mut view = view();
        type_text(&mut view, "/exit");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::Submit(text)) => assert_eq!(text, "/exit"),
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn enter_with_args_preserves_them() {
        let mut view = view();
        type_text(&mut view, "/clear hi");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::Submit(text)) => assert_eq!(text, "/clear hi"),
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn enter_prefix_fills_command() {
        let mut view = view();
        type_text(&mut view, "/ex");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        assert!(matches!(result, KeyResult::Handled));
        assert_eq!(view.textarea.lines()[0], "/exit ");
        assert!(view.slash_command.is_open());
    }

    #[test]
    fn tab_fills_selected() {
        let mut view = view();
        type_text(&mut view, "/c");
        let result = view.handle_key(key(KeyCode::Tab), &State::new());
        assert!(matches!(result, KeyResult::Handled));
        assert_eq!(view.textarea.lines()[0], "/clear ");
    }

    #[test]
    fn arrows_change_selection() {
        let mut view = view();
        type_text(&mut view, "/");
        assert_eq!(view.slash_command.selected_command(), Some(0));
        view.handle_key(key(KeyCode::Down), &State::new());
        assert_eq!(view.slash_command.selected_command(), Some(1));
        view.handle_key(key(KeyCode::Down), &State::new());
        assert_eq!(view.slash_command.selected_command(), Some(2));
        view.handle_key(key(KeyCode::Down), &State::new());
        assert_eq!(view.slash_command.selected_command(), Some(0));
    }

    #[test]
    fn enter_without_match_submits_typed_text() {
        let mut view = view();
        type_text(&mut view, "/nope");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::Submit(text)) => assert_eq!(text, "/nope"),
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn busy_allows_typing() {
        let mut view = view();
        let state = State { busy: true };
        let result = view.handle_key(key(KeyCode::Char('x')), &state);
        assert!(matches!(result, KeyResult::Handled));
        assert_eq!(view.textarea.lines()[0], "x");
    }

    #[test]
    fn busy_enter_queues_text() {
        let mut view = view();
        type_text(&mut view, "hello");
        let state = State { busy: true };
        let result = view.handle_key(key(KeyCode::Enter), &state);
        match result {
            KeyResult::Action(Action::Queue(text)) => assert_eq!(text, "hello"),
            _ => panic!("expected queue"),
        }
        assert_eq!(view.queue_len(), 1);
        assert!(view.textarea.lines()[0].is_empty());
    }

    #[test]
    fn busy_enter_empty_does_not_queue() {
        let mut view = view();
        let state = State { busy: true };
        let result = view.handle_key(key(KeyCode::Enter), &state);
        assert!(matches!(result, KeyResult::Handled));
        assert_eq!(view.queue_len(), 0);
    }

    #[test]
    fn busy_alt_enter_inserts_newline() {
        let mut view = view();
        type_text(&mut view, "a");
        let state = State { busy: true };
        let result = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT), &state);
        assert!(matches!(result, KeyResult::Handled));
        assert_eq!(view.textarea.lines()[0], "a");
        assert_eq!(view.textarea.lines().len(), 2);
    }

    #[test]
    fn busy_exact_slash_submits_immediately() {
        let mut view = view();
        type_text(&mut view, "/clear");
        let state = State { busy: true };
        let result = view.handle_key(key(KeyCode::Enter), &state);
        match result {
            KeyResult::Action(Action::Submit(text)) => assert_eq!(text, "/clear"),
            _ => panic!("expected submit"),
        }
        assert_eq!(view.queue_len(), 0);
    }

    #[test]
    fn drain_pending_returns_in_order() {
        let mut view = view();
        type_text(&mut view, "one");
        view.handle_key(key(KeyCode::Enter), &State { busy: true });
        type_text(&mut view, "two");
        view.handle_key(key(KeyCode::Enter), &State { busy: true });
        assert_eq!(view.queue_len(), 2);
        assert_eq!(
            view.drain_pending(),
            vec!["one".to_string(), "two".to_string()]
        );
        assert_eq!(view.queue_len(), 0);
    }

    #[test]
    fn pending_returns_queued_in_order() {
        let mut view = view();
        type_text(&mut view, "one");
        view.handle_key(key(KeyCode::Enter), &State { busy: true });
        type_text(&mut view, "two");
        view.handle_key(key(KeyCode::Enter), &State { busy: true });
        assert_eq!(view.pending(), &["one".to_string(), "two".to_string()]);
    }

    fn open_picker(view: &mut InputView) {
        type_text(view, "/model");
        view.handle_key(key(KeyCode::Enter), &State::new());
        assert!(view.model_picker.is_open());
    }

    #[test]
    fn model_picker_opens_on_enter() {
        let mut view = view();
        type_text(&mut view, "/model");
        assert!(!view.model_picker.is_open());

        view.handle_key(key(KeyCode::Enter), &State::new());
        assert!(view.model_picker.is_open());
        assert_eq!(view.textarea.lines()[0], "/model ");
        assert_eq!(view.model_picker.matches(), vec![0, 1, 2]);
    }

    #[test]
    fn model_picker_seeds_filter_from_typed_fragment() {
        let mut view = view();
        type_text(&mut view, "/model g");
        view.handle_key(key(KeyCode::Enter), &State::new());
        assert!(view.model_picker.is_open());
        assert_eq!(view.model_picker.filter, "g");
        assert_eq!(view.model_picker.matches(), vec![0, 1]);
    }

    #[test]
    fn model_filter_typing_and_backspace() {
        let mut view = view();
        open_picker(&mut view);
        for ch in ['d', 'e', 'e', 'p'] {
            view.handle_key(key(KeyCode::Char(ch)), &State::new());
        }
        assert_eq!(view.model_picker.filter, "deep");
        assert_eq!(view.model_picker.matches(), vec![2]);

        view.handle_key(key(KeyCode::Backspace), &State::new());
        assert_eq!(view.model_picker.filter, "dee");
        assert_eq!(view.model_picker.matches(), vec![2]);

        for _ in 0..3 {
            view.handle_key(key(KeyCode::Backspace), &State::new());
        }
        assert_eq!(view.model_picker.filter, "");
        assert_eq!(view.model_picker.matches(), vec![0, 1, 2]);
    }

    #[test]
    fn model_enter_advances_then_effort_submits() {
        let mut view = view();
        type_text(&mut view, "/model deep");
        view.handle_key(key(KeyCode::Enter), &State::new());
        assert!(view.model_picker.is_open());
        assert_eq!(view.model_picker.matches(), vec![2]);

        // First Enter confirms the model and advances to the effort stage.
        view.handle_key(key(KeyCode::Enter), &State::new());
        // Second Enter picks the first effort (none).
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::Submit(text)) => {
                assert_eq!(text, "/model deepseek-chat none");
            }
            _ => panic!("expected submit"),
        }
        assert!(!view.model_picker.is_open());
    }

    #[test]
    fn effort_arrows_select_low() {
        let mut view = view();
        type_text(&mut view, "/model deep");
        view.handle_key(key(KeyCode::Enter), &State::new());
        view.handle_key(key(KeyCode::Enter), &State::new());
        view.handle_key(key(KeyCode::Down), &State::new());
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::Submit(text)) => {
                assert_eq!(text, "/model deepseek-chat low");
            }
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn effort_keep_current_submits_without_effort() {
        let mut view = view();
        open_picker(&mut view);
        view.handle_key(key(KeyCode::Enter), &State::new());
        for _ in 0..4 {
            view.handle_key(key(KeyCode::Down), &State::new());
        }
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::Submit(text)) => assert_eq!(text, "/model gpt-4o"),
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn effort_esc_returns_to_model_stage() {
        let mut view = view();
        open_picker(&mut view);
        view.handle_key(key(KeyCode::Enter), &State::new());
        let result = view.handle_key(key(KeyCode::Esc), &State::new());
        assert!(matches!(result, KeyResult::Handled));
        assert!(view.model_picker.is_open());
        assert_eq!(view.model_picker.matches(), vec![0, 1, 2]);
    }

    #[test]
    fn esc_closes_picker_and_restores_input() {
        let mut view = view();
        open_picker(&mut view);
        let result = view.handle_key(key(KeyCode::Esc), &State::new());
        assert!(matches!(result, KeyResult::Handled));
        assert!(!view.model_picker.is_open());
        assert_eq!(view.textarea.lines()[0], "/model");
    }

    #[test]
    fn manual_fast_path_submits_without_picker() {
        let mut view = view();
        type_text(&mut view, "/model gpt-4o high");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::Submit(text)) => {
                assert_eq!(text, "/model gpt-4o high");
            }
            _ => panic!("expected submit"),
        }
        assert!(!view.model_picker.is_open());
    }

    #[test]
    fn update_models_refreshes_picker() {
        let mut view = InputView::new(commands());
        let ev = AppEvent::ModelsUpdated {
            app_id: oven_app::AppId(1),
            models: vec![("kimi-k2".into(), "Moonshot".into())],
        };
        view.on_event(&ev, &mut State::new());
        type_text(&mut view, "/model");
        view.handle_key(key(KeyCode::Enter), &State::new());
        assert_eq!(view.model_picker.matches(), vec![0]);
    }
}

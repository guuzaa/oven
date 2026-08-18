use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use oven_app::AppEvent;
use oven_app::config::ProviderConfig;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use tui_textarea::{TextArea, WrapMode};
use unicode_width::UnicodeWidthStr;

const PROMPT_COLS: u16 = 2;
const MAX_INPUT_ROWS: u16 = 8;

use super::component::{Action, Component, KeyResult, State};
use super::model_picker::{ModelPicker, ModelPickerAction};
use super::setup_wizard::{SetupWizard, SetupWizardAction};
use super::slash_command_popup::{SlashCommandPopup, SlashCommandPopupAction};
use super::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    None,
    Slash,
    Model,
    Setup,
}

pub struct InputView {
    textarea: TextArea<'static>,
    slash_command: SlashCommandPopup,
    model_picker: ModelPicker,
    setup: SetupWizard,
}

impl InputView {
    pub fn new(commands: Vec<(String, String)>, provider: ProviderConfig) -> Self {
        Self {
            textarea: new_textarea(),
            slash_command: SlashCommandPopup::new(commands),
            model_picker: ModelPicker::new(Vec::new()),
            setup: SetupWizard::new(provider),
        }
    }

    pub fn height(&mut self, area_width: u16) -> u16 {
        let text_width = area_width.saturating_sub(PROMPT_COLS);
        self.textarea.measure(text_width).preferred_rows
    }

    pub fn clear(&mut self) {
        self.textarea = new_textarea();
        self.slash_command.close();
        self.model_picker.close();
        self.setup.close();
    }

    pub fn overlay(&self) -> Overlay {
        if self.setup.is_open() {
            Overlay::Setup
        } else if self.model_picker.is_open() {
            Overlay::Model
        } else if self.slash_command.is_open() {
            Overlay::Slash
        } else {
            Overlay::None
        }
    }

    pub fn overlay_height(&self) -> u16 {
        match self.overlay() {
            Overlay::None => 0,
            Overlay::Setup => self.setup.height(),
            Overlay::Model => self.model_picker.height(),
            Overlay::Slash => self.slash_command.height(),
        }
    }

    pub fn draw_overlay(&mut self, f: &mut Frame<'_>, area: Rect) {
        match self.overlay() {
            Overlay::None => {}
            Overlay::Setup => self.setup.draw(f, area),
            Overlay::Model => self.model_picker.draw(f, area),
            Overlay::Slash => self.slash_command.draw(f, area),
        }
    }

    pub(crate) fn open_setup(&mut self) {
        self.setup.open();
        self.fill_command("/setup ");
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if self.setup.is_open() {
            self.setup.paste(text);
            return;
        }
        if self.model_picker.is_open() {
            self.model_picker.paste(text);
            return;
        }
        self.textarea.insert_str(text);
        self.slash_command.refresh(&self.text());
    }

    pub(crate) fn set_text(&mut self, text: &str) {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let row = lines.len().saturating_sub(1);
        let col = lines.last().map(|l| l.width()).unwrap_or(0);
        self.textarea.set_lines(lines, (row, col));
        self.slash_command.refresh(&self.text());
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
    fn on_event(&mut self, ev: &AppEvent) {
        match ev {
            AppEvent::ModelsUpdated { models, .. } => {
                self.model_picker.update_models(models.clone());
            }
            AppEvent::ProviderUpdated { provider, .. } => {
                self.setup.set_current(provider.clone());
            }
            AppEvent::Rewound {
                text: Some(text), ..
            } => {
                self.set_text(text);
            }
            AppEvent::Rewound { .. } => {}
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent, state: &State) -> KeyResult {
        let text = self.text();
        self.slash_command.refresh(&text);

        if self.setup.is_open() {
            return match self.setup.handle_key(key) {
                SetupWizardAction::Handled => KeyResult::Handled,
                SetupWizardAction::Submit(text) => {
                    self.clear();
                    submit_command(text)
                }
                SetupWizardAction::Close => {
                    self.fill_command("/setup");
                    KeyResult::Handled
                }
            };
        }

        if self.model_picker.is_open() {
            return match self.model_picker.handle_key(key) {
                ModelPickerAction::Handled => KeyResult::Handled,
                ModelPickerAction::Submit(text) => {
                    self.clear();
                    submit_command(text)
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
                    if setup_opens(&text) {
                        self.setup.open();
                        self.fill_command("/setup ");
                        return KeyResult::Action(Action::QuietSubmit(text));
                    }
                    // `/model` (with at most one fragment) opens the picker
                    // instead of submitting; two or more args keep the manual
                    // fast path. Bare `/model` still runs so the Reply
                    // (current model) can show below the status bar.
                    if let Some(filter) = model_filter_from(&text) {
                        self.model_picker.open(&filter);
                        self.fill_command("/model ");
                        return if filter.is_empty() {
                            KeyResult::Action(Action::QuietSubmit(text))
                        } else {
                            KeyResult::Handled
                        };
                    }
                    self.clear();
                    submit_command(text)
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
                    if is_model_or_setup(&text) {
                        self.clear();
                        submit_command(text)
                    } else {
                        self.clear();
                        KeyResult::Action(Action::Queue(text))
                    }
                } else {
                    self.clear();
                    submit_command(text)
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
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(PROMPT_COLS), Constraint::Min(1)])
            .split(area);
        let prompt = if state.busy { "⋅ " } else { "› " };
        f.render_widget(
            Paragraph::new(Span::styled(prompt, theme::user())),
            chunks[0],
        );
        if self.setup.is_open() {
            draw_setup_prompt(f, chunks[1], &self.setup);
            return;
        }
        self.textarea.set_style(Style::default());
        self.textarea
            .set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        self.textarea.set_cursor_line_style(Style::default());
        f.render_widget(&self.textarea, chunks[1]);
    }
}

/// If `text` is a `/model` command with at most one argument (`/model` or
/// `/model <fragment>`), return the fragment to seed the picker filter with.
/// Two or more arguments return `None` so the line submits directly.
fn is_model_or_setup(text: &str) -> bool {
    let trimmed = text.trim();
    let Some(body) = trimmed.strip_prefix('/') else {
        return false;
    };
    matches!(
        body.split_whitespace().next(),
        Some(cmd) if cmd.eq_ignore_ascii_case("model") || cmd.eq_ignore_ascii_case("setup")
    )
}

fn submit_command(text: String) -> KeyResult {
    if is_model_or_setup(&text) {
        KeyResult::Action(Action::QuietSubmit(text))
    } else {
        KeyResult::Action(Action::Submit(text))
    }
}

fn setup_opens(text: &str) -> bool {
    let trimmed = text.trim();
    let Some(body) = trimmed.strip_prefix('/') else {
        return false;
    };
    let mut words = body.split_whitespace();
    matches!(words.next(), Some(cmd) if cmd.eq_ignore_ascii_case("setup")) && words.next().is_none()
}

pub(crate) fn display_user_input(text: &str) -> String {
    if !text.trim_start().starts_with("/setup") {
        return text.to_string();
    }
    text.split(' ')
        .map(|token| {
            token
                .strip_prefix("api_key=")
                .filter(|v| !v.is_empty())
                .map(|_| "api_key=***")
                .unwrap_or(token)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn draw_setup_prompt(f: &mut Frame<'_>, area: Rect, setup: &SetupWizard) {
    if let Some(mask) = setup.prompt_mask() {
        f.render_widget(Paragraph::new(Span::raw(mask.clone())), area);
        let col = (mask.chars().count() as u16).min(area.width.saturating_sub(1));
        f.set_cursor_position((area.x.saturating_add(col), area.y));
        return;
    }
    f.render_widget(
        Paragraph::new(Span::styled(setup.prompt_hint(), theme::dim())),
        area,
    );
}

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
    ta.set_wrap_mode(WrapMode::WordOrGlyph);
    ta.set_min_rows(1);
    ta.set_max_rows(MAX_INPUT_ROWS);
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
            ("setup".into(), "Configure provider.".into()),
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
        let mut view = InputView::new(commands(), ProviderConfig::default());
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
        assert_eq!(view.overlay_height(), 4);
        assert_eq!(view.overlay(), Overlay::Slash);
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
        assert_eq!(view.slash_command.selected_command(), Some(3));
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
        let state = State {
            busy: true,
            ..State::new()
        };
        let result = view.handle_key(key(KeyCode::Char('x')), &state);
        assert!(matches!(result, KeyResult::Handled));
        assert_eq!(view.textarea.lines()[0], "x");
    }

    #[test]
    fn busy_enter_queues_text() {
        let mut view = view();
        type_text(&mut view, "hello");
        let state = State {
            busy: true,
            ..State::new()
        };
        let result = view.handle_key(key(KeyCode::Enter), &state);
        match result {
            KeyResult::Action(Action::Queue(text)) => assert_eq!(text, "hello"),
            _ => panic!("expected queue"),
        }
        assert!(view.textarea.lines()[0].is_empty());
    }

    #[test]
    fn busy_enter_empty_does_not_queue() {
        let mut view = view();
        let state = State {
            busy: true,
            ..State::new()
        };
        let result = view.handle_key(key(KeyCode::Enter), &state);
        assert!(matches!(result, KeyResult::Handled));
    }

    #[test]
    fn busy_alt_enter_inserts_newline() {
        let mut view = view();
        type_text(&mut view, "a");
        let state = State {
            busy: true,
            ..State::new()
        };
        let result = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT), &state);
        assert!(matches!(result, KeyResult::Handled));
        assert_eq!(view.textarea.lines()[0], "a");
        assert_eq!(view.textarea.lines().len(), 2);
    }

    #[test]
    fn busy_exact_slash_submits_immediately() {
        let mut view = view();
        type_text(&mut view, "/clear");
        let state = State {
            busy: true,
            ..State::new()
        };
        let result = view.handle_key(key(KeyCode::Enter), &state);
        match result {
            KeyResult::Action(Action::Submit(text)) => assert_eq!(text, "/clear"),
            _ => panic!("expected submit"),
        }
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

        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::QuietSubmit(text)) => assert_eq!(text, "/model"),
            _ => panic!("expected QuietSubmit /model"),
        }
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
        assert_eq!(view.model_picker.filter(), "g");
        assert_eq!(view.model_picker.matches(), vec![0, 1]);
    }

    #[test]
    fn model_filter_typing_and_backspace() {
        let mut view = view();
        open_picker(&mut view);
        for ch in ['d', 'e', 'e', 'p'] {
            view.handle_key(key(KeyCode::Char(ch)), &State::new());
        }
        assert_eq!(view.model_picker.filter(), "deep");
        assert_eq!(view.model_picker.matches(), vec![2]);

        view.handle_key(key(KeyCode::Backspace), &State::new());
        assert_eq!(view.model_picker.filter(), "dee");
        assert_eq!(view.model_picker.matches(), vec![2]);

        for _ in 0..3 {
            view.handle_key(key(KeyCode::Backspace), &State::new());
        }
        assert_eq!(view.model_picker.filter(), "");
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
            KeyResult::Action(Action::QuietSubmit(text)) => {
                assert_eq!(text, "/model deepseek-chat none");
            }
            _ => panic!("expected QuietSubmit"),
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
            KeyResult::Action(Action::QuietSubmit(text)) => {
                assert_eq!(text, "/model deepseek-chat low");
            }
            _ => panic!("expected QuietSubmit"),
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
            KeyResult::Action(Action::QuietSubmit(text)) => assert_eq!(text, "/model gpt-4o"),
            _ => panic!("expected QuietSubmit"),
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
            KeyResult::Action(Action::QuietSubmit(text)) => {
                assert_eq!(text, "/model gpt-4o high");
            }
            _ => panic!("expected QuietSubmit"),
        }
        assert!(!view.model_picker.is_open());
    }

    #[test]
    fn update_models_refreshes_picker() {
        let mut view = InputView::new(commands(), ProviderConfig::default());
        let ev = AppEvent::ModelsUpdated {
            app_id: oven_app::AppId(1),
            models: vec![("kimi-k2".into(), "Moonshot".into())],
        };
        view.on_event(&ev);
        type_text(&mut view, "/model");
        view.handle_key(key(KeyCode::Enter), &State::new());
        assert_eq!(view.model_picker.matches(), vec![0]);
    }

    #[test]
    fn set_text_replaces_single_line_and_places_cursor() {
        let mut view = view();
        type_text(&mut view, "draft");
        view.set_text("hello");
        assert_eq!(view.textarea.lines()[0], "hello");
        assert_eq!(view.textarea.cursor(), (0, 5));
    }

    #[test]
    fn set_text_supports_multiline() {
        let mut view = view();
        view.set_text("line one\nline two");
        assert_eq!(view.textarea.lines(), &["line one", "line two"]);
        assert_eq!(view.textarea.cursor(), (1, 8));
    }

    #[test]
    fn rewound_event_restores_input_text() {
        let mut view = view();
        type_text(&mut view, "draft");
        let ev = AppEvent::Rewound {
            app_id: oven_app::AppId(1),
            text: Some("restored".into()),
            messages: Vec::new(),
            usage: oven_llm::Usage::default(),
        };
        view.on_event(&ev);
        assert_eq!(view.textarea.lines()[0], "restored");
    }

    #[test]
    fn setup_wizard_opens_on_enter() {
        let mut view = view();
        type_text(&mut view, "/setup");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::QuietSubmit(text)) => assert_eq!(text, "/setup"),
            _ => panic!("expected QuietSubmit /setup"),
        }
        assert!(view.setup.is_open());
        assert_eq!(view.textarea.lines()[0], "/setup ");
    }

    #[test]
    fn open_setup_opens_wizard() {
        let mut view = view();
        view.open_setup();
        assert!(view.setup.is_open());
        assert_eq!(view.textarea.lines()[0], "/setup ");
    }

    #[test]
    fn setup_with_args_submits_without_wizard() {
        let mut view = view();
        type_text(&mut view, "/setup kind=completions");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::QuietSubmit(text)) => {
                assert_eq!(text, "/setup kind=completions");
            }
            _ => panic!("expected QuietSubmit"),
        }
        assert!(!view.setup.is_open());
    }

    #[test]
    fn setup_esc_closes_and_restores_input() {
        let mut view = view();
        type_text(&mut view, "/setup");
        view.handle_key(key(KeyCode::Enter), &State::new());
        let result = view.handle_key(key(KeyCode::Esc), &State::new());
        assert!(matches!(result, KeyResult::Handled));
        assert!(!view.setup.is_open());
        assert_eq!(view.textarea.lines()[0], "/setup");
    }

    #[test]
    fn model_and_setup_are_silent_slash_commands() {
        assert!(is_model_or_setup("/model"));
        assert!(is_model_or_setup("/MODEL gpt-4o"));
        assert!(is_model_or_setup("/setup name=x"));
        assert!(!is_model_or_setup("/clear"));
        assert!(!is_model_or_setup("hello"));
    }

    #[test]
    fn busy_model_submits_quietly_without_queue() {
        let mut view = view();
        type_text(&mut view, "/model gpt-4o high");
        let result = view.handle_key(
            key(KeyCode::Enter),
            &State {
                busy: true,
                ..State::new()
            },
        );
        match result {
            KeyResult::Action(Action::QuietSubmit(text)) => {
                assert_eq!(text, "/model gpt-4o high");
            }
            _ => panic!("expected QuietSubmit"),
        }
    }

    #[test]
    fn display_user_input_redacts_api_key() {
        assert_eq!(
            display_user_input("/setup name=deepseek api_key=sk-secret"),
            "/setup name=deepseek api_key=***"
        );
        assert_eq!(display_user_input("hello"), "hello");
    }

    #[test]
    fn height_is_one_when_empty() {
        let mut view = view();
        assert_eq!(view.height(80), 1);
    }

    #[test]
    fn height_grows_when_line_wraps() {
        let mut view = view();
        type_text(&mut view, &"x".repeat(20));
        assert_eq!(view.height(12), 2);
        assert_eq!(view.height(22), 1);
    }

    #[test]
    fn height_counts_hard_newlines() {
        let mut view = view();
        view.set_text("a\nb\nc");
        assert_eq!(view.height(80), 3);
    }

    #[test]
    fn height_clamps_to_max_rows() {
        let mut view = view();
        view.set_text(
            &(0..20)
                .map(|i| format!("line{i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        assert_eq!(view.height(80), MAX_INPUT_ROWS);
    }

    #[test]
    fn height_wraps_wide_chars() {
        let mut view = view();
        type_text(&mut view, &"你".repeat(10));
        assert_eq!(view.height(12), 2);
        assert_eq!(view.height(22), 1);
    }
}

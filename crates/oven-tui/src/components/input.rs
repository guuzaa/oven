use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use oven_app::AgentMode;
use oven_app::FileMentions;
use oven_app::config::ProviderConfig;
use oven_app::{AppEvent, AppEventKind, StateChange, StateEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Style;
use ratatui::text::Span;
use ratatui::widgets::{Block, Paragraph};
use tui_textarea::{CursorRenderMode, TextArea, WrapMode};

const PROMPT_COLS: u16 = 2;
const MAX_INPUT_ROWS: u16 = 8;
const BORDER_COLS: u16 = 2;
const BORDER_ROWS: u16 = 2;

use super::component::{Action, Component, KeyResult, State};
use super::file_mention_popup::{FileMentionPopup, FileMentionPopupAction};
use super::model_picker::{ModelPicker, ModelPickerAction};
use super::setup_wizard::{SetupWizard, SetupWizardAction};
use super::slash_command_popup::{SlashCommandPopup, SlashCommandPopupAction};
use super::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    None,
    Slash,
    Mention,
    Model,
    Setup,
}

pub struct InputView {
    textarea: TextArea<'static>,
    slash_command: SlashCommandPopup,
    file_mention: FileMentionPopup,
    model_picker: ModelPicker,
    setup: SetupWizard,
    root: PathBuf,
    mentions: Option<FileMentions>,
}

impl InputView {
    pub fn new(commands: Vec<(String, String)>, provider: ProviderConfig) -> Self {
        Self {
            textarea: new_textarea(),
            slash_command: SlashCommandPopup::new(commands),
            file_mention: FileMentionPopup::new(),
            model_picker: ModelPicker::new(Vec::new()),
            setup: SetupWizard::new(provider),
            root: PathBuf::new(),
            mentions: None,
        }
    }

    pub fn with_root(mut self, root: impl AsRef<Path>) -> Self {
        self.root = root.as_ref().to_path_buf();
        self
    }

    #[cfg(test)]
    fn with_files(mut self, files: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.mentions = Some(FileMentions::from_files(files));
        self
    }

    pub fn height(&mut self, area_width: u16) -> u16 {
        let bordered = fits_border(Rect::new(0, 0, area_width, BORDER_ROWS + 1));
        let inner_w = if bordered {
            area_width.saturating_sub(BORDER_COLS)
        } else {
            area_width
        };
        let text_width = inner_w.saturating_sub(PROMPT_COLS);
        let rows = self.textarea.measure(text_width).preferred_rows;
        if bordered {
            rows.saturating_add(BORDER_ROWS)
        } else {
            rows
        }
    }

    pub fn clear(&mut self) {
        self.textarea = new_textarea();
        self.slash_command.close();
        self.file_mention.close();
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
        } else if self.file_mention.is_open() {
            Overlay::Mention
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
            Overlay::Mention => self.file_mention.height(),
        }
    }

    pub fn draw_overlay(&mut self, f: &mut Frame<'_>, area: Rect) {
        match self.overlay() {
            Overlay::None => {}
            Overlay::Setup => self.setup.draw(f, area),
            Overlay::Model => self.model_picker.draw(f, area),
            Overlay::Slash => self.slash_command.draw(f, area),
            Overlay::Mention => self.file_mention.draw(f, area),
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
        self.refresh_popups();
    }

    pub(crate) fn set_text(&mut self, text: &str) {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let row = lines.len().saturating_sub(1);
        let col = lines.last().map(|l| l.chars().count()).unwrap_or(0);
        self.textarea.set_lines(lines, (row, col));
        self.refresh_popups();
    }

    fn text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    fn fill_command(&mut self, text: &str) {
        let col = text.chars().count();
        self.textarea.set_lines(vec![text.to_string()], (0, col));
        self.refresh_popups();
    }

    fn splice(&mut self, before: &str, insert: &str, after: &str) {
        let text = format!("{before}{insert}{after}");
        let prefix = format!("{before}{insert}");
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let prefix_lines: Vec<&str> = prefix.split('\n').collect();
        let row = prefix_lines.len().saturating_sub(1);
        let col = prefix_lines.last().map(|l| l.chars().count()).unwrap_or(0);
        self.textarea.set_lines(lines, (row, col));
        self.refresh_popups();
    }

    fn refresh_popups(&mut self) {
        let text = self.text();
        self.slash_command.refresh(&text);
        if self.slash_command.is_open() || self.setup.is_open() || self.model_picker.is_open() {
            self.file_mention.close();
            return;
        }
        let cursor = cursor_byte(&text, self.textarea.cursor());
        let mentions = self
            .mentions
            .get_or_insert_with(|| FileMentions::open(&self.root));
        self.file_mention.refresh(&text, cursor, mentions);
    }

    fn border_style(&self, state: &State) -> Style {
        if state.mode == AgentMode::Plan {
            theme::mode()
        } else if self.textarea.lines().iter().any(|line| !line.is_empty()) {
            theme::border_active()
        } else {
            theme::border_idle()
        }
    }
}

impl Component for InputView {
    fn on_event(&mut self, ev: &AppEvent) {
        let AppEventKind::StateChanged(StateEvent { change, .. }) = &ev.kind else {
            return;
        };
        match change {
            StateChange::ModelsChanged { models } => {
                self.model_picker.update_models(models.clone());
            }
            StateChange::ProviderChanged { provider } => {
                self.setup.set_current(provider.clone());
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent, state: &State) -> KeyResult {
        self.refresh_popups();

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

        if self.file_mention.is_open()
            && let Some(action) = self.file_mention.handle_key(key)
        {
            return match action {
                FileMentionPopupAction::Handled => KeyResult::Handled,
                FileMentionPopupAction::Fill {
                    before,
                    insert,
                    after,
                } => {
                    self.splice(&before, &insert, &after);
                    KeyResult::Handled
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
                self.refresh_popups();
                KeyResult::Handled
            }
        }
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect, state: &State) {
        let inner = draw_composer_border(f, area, self.border_style(state));
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(PROMPT_COLS), Constraint::Min(1)])
            .split(inner);
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
        self.textarea.set_cursor_line_style(Style::default());
        f.render_widget(&self.textarea, chunks[1]);
        if let Some(pos) = self.textarea.rendered_cursor_position() {
            f.set_cursor_position(pos);
        }
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

fn cursor_byte(text: &str, (row, col): (usize, usize)) -> usize {
    let mut offset = 0;
    for (i, line) in text.split('\n').enumerate() {
        if i == row {
            return offset + char_index_to_byte(line, col);
        }
        offset += line.len() + 1;
    }
    text.len()
}

fn char_index_to_byte(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map(|(i, _)| i)
        .unwrap_or(line.len())
}

fn fits_border(area: Rect) -> bool {
    area.height > BORDER_ROWS && area.width > PROMPT_COLS + BORDER_COLS
}

fn draw_composer_border(f: &mut Frame<'_>, area: Rect, style: Style) -> Rect {
    if !fits_border(area) {
        return area;
    }
    let block = Block::bordered()
        .border_type(theme::border_type())
        .border_style(style);
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

fn new_textarea() -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_cursor_line_style(Style::default());
    ta.set_cursor_render_mode(CursorRenderMode::Hidden);
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
        let ev = AppEvent::state_changed(oven_app::StateChange::ModelsChanged {
            models: vec![("kimi-k2".into(), "Moonshot".into())],
        });
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
    fn history_changed_does_not_touch_input() {
        let mut view = view();
        type_text(&mut view, "draft");
        let ev = AppEvent::state_changed(oven_app::StateChange::HistoryChanged { revision: 1 });
        view.on_event(&ev);
        assert_eq!(view.textarea.lines()[0], "draft");
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
        type_text(&mut view, "/setup name=deepseek");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::QuietSubmit(text)) => {
                assert_eq!(text, "/setup name=deepseek");
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
    fn height_includes_border_when_empty() {
        let mut view = view();
        assert_eq!(view.height(80), 1 + BORDER_ROWS);
    }

    #[test]
    fn height_grows_when_line_wraps() {
        let mut view = view();
        type_text(&mut view, &"x".repeat(20));
        assert_eq!(view.height(12), 3 + BORDER_ROWS);
        assert_eq!(view.height(22), 2 + BORDER_ROWS);
        assert_eq!(view.height(24), 1 + BORDER_ROWS);
    }

    #[test]
    fn height_counts_hard_newlines() {
        let mut view = view();
        view.set_text("a\nb\nc");
        assert_eq!(view.height(80), 3 + BORDER_ROWS);
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
        assert_eq!(view.height(80), MAX_INPUT_ROWS + BORDER_ROWS);
    }

    #[test]
    fn height_wraps_wide_chars() {
        let mut view = view();
        type_text(&mut view, &"你".repeat(10));
        assert_eq!(view.height(12), 3 + BORDER_ROWS);
        assert_eq!(view.height(22), 2 + BORDER_ROWS);
        assert_eq!(view.height(24), 1 + BORDER_ROWS);
    }

    #[test]
    fn height_skips_border_when_too_narrow() {
        let mut view = view();
        assert_eq!(view.height(PROMPT_COLS + BORDER_COLS), 1);
    }

    fn mention_view() -> InputView {
        view().with_files(["README.md", "src/app.rs", "src/lib.rs"])
    }

    #[test]
    fn cursor_byte_uses_char_index_not_width() {
        for (text, at_end) in [
            ("hello", 5),
            ("你好 @", 4),
            ("こんにちは @", 7),
            ("¿dónde está @", 13),
        ] {
            assert_eq!(cursor_byte(text, (0, at_end)), text.len(), "{text}");
        }
        assert_eq!(cursor_byte("你好 @", (0, 3)), "你好 ".len());
        assert_eq!(cursor_byte("こんにちは @", (0, 6)), "こんにちは ".len());
        assert_eq!(cursor_byte("niño @", (0, 5)), "niño ".len());
    }

    #[test]
    fn mention_opens_on_at() {
        let mut view = mention_view();
        type_text(&mut view, "@");
        assert!(view.file_mention.is_open());
        assert_eq!(view.overlay(), Overlay::Mention);
        assert_eq!(view.overlay_height(), 3);
    }

    #[test]
    fn mention_opens_after_non_ascii_and_space() {
        for typed in ["你好 @", "こんにちは @", "¿dónde está @"] {
            let mut view = mention_view();
            type_text(&mut view, typed);
            assert!(view.file_mention.is_open(), "{typed}");
            assert_eq!(view.overlay(), Overlay::Mention, "{typed}");
            assert_eq!(
                view.textarea.cursor(),
                (0, typed.chars().count()),
                "{typed}"
            );
        }
    }

    #[test]
    fn mention_fill_keeps_non_ascii_prefix() {
        for (typed, filled) in [
            ("看看 @li", "看看 @src/lib.rs "),
            ("見て @li", "見て @src/lib.rs "),
            ("añade @li", "añade @src/lib.rs "),
        ] {
            let mut view = mention_view();
            type_text(&mut view, typed);
            view.handle_key(key(KeyCode::Tab), &State::new());
            assert_eq!(view.textarea.lines()[0], filled, "{typed}");
            assert_eq!(
                view.textarea.cursor(),
                (0, filled.chars().count()),
                "{typed}"
            );
        }
    }

    #[test]
    fn mention_filters_by_query() {
        let mut view = mention_view();
        type_text(&mut view, "@lib");
        assert_eq!(view.file_mention.matches(), &["src/lib.rs"]);
    }

    #[test]
    fn mention_tab_fills_selected() {
        let mut view = mention_view();
        type_text(&mut view, "see @li");
        let result = view.handle_key(key(KeyCode::Tab), &State::new());
        assert!(matches!(result, KeyResult::Handled));
        assert_eq!(view.textarea.lines()[0], "see @src/lib.rs ");
        assert!(!view.file_mention.is_open());
    }

    #[test]
    fn mention_fill_keeps_trailing_text() {
        let mut view = mention_view();
        type_text(&mut view, "see @li please");
        for _ in 0.." please".len() {
            view.handle_key(key(KeyCode::Left), &State::new());
        }
        assert!(view.file_mention.is_open());
        view.handle_key(key(KeyCode::Tab), &State::new());
        assert_eq!(view.textarea.lines()[0], "see @src/lib.rs please");
    }

    #[test]
    fn mention_esc_closes() {
        let mut view = mention_view();
        type_text(&mut view, "@");
        let result = view.handle_key(key(KeyCode::Esc), &State::new());
        assert!(matches!(result, KeyResult::Handled));
        assert!(!view.file_mention.is_open());
        assert_eq!(view.textarea.lines()[0], "@");
    }

    #[test]
    fn mention_closes_when_at_deleted() {
        let mut view = mention_view();
        type_text(&mut view, "@l");
        view.handle_key(key(KeyCode::Backspace), &State::new());
        view.handle_key(key(KeyCode::Backspace), &State::new());
        assert!(!view.file_mention.is_open());
    }

    #[test]
    fn slash_takes_priority_over_mention() {
        let mut view = mention_view();
        type_text(&mut view, "/");
        assert_eq!(view.overlay(), Overlay::Slash);
        assert!(!view.file_mention.is_open());
    }

    #[test]
    fn email_does_not_open_mention() {
        let mut view = mention_view();
        type_text(&mut view, "foo@bar");
        assert!(!view.file_mention.is_open());
        assert_eq!(view.overlay(), Overlay::None);
    }

    #[test]
    fn mention_enter_prefix_fills() {
        let mut view = mention_view();
        type_text(&mut view, "@li");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        assert!(matches!(result, KeyResult::Handled));
        assert_eq!(view.textarea.lines()[0], "@src/lib.rs ");
    }

    #[test]
    fn mention_enter_exact_submits() {
        let mut view = mention_view();
        type_text(&mut view, "@src/lib.rs");
        let result = view.handle_key(key(KeyCode::Enter), &State::new());
        match result {
            KeyResult::Action(Action::Submit(text)) => assert_eq!(text, "@src/lib.rs"),
            _ => panic!("expected submit"),
        }
    }

    #[test]
    fn mention_arrows_change_selection() {
        let mut view = mention_view();
        type_text(&mut view, "@");
        assert_eq!(view.file_mention.matches()[0], "README.md");
        view.handle_key(key(KeyCode::Down), &State::new());
        view.handle_key(key(KeyCode::Tab), &State::new());
        assert_eq!(view.textarea.lines()[0], "@src/app.rs ");
    }

    fn render(
        view: &mut InputView,
        width: u16,
        height: u16,
        state: &State,
    ) -> (String, ratatui::buffer::Buffer) {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                view.draw(f, f.area(), state);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        (out, buf)
    }

    #[test]
    fn draw_paints_rounded_border_around_prompt() {
        let mut view = view();
        let (out, buf) = render(&mut view, 40, 3, &State::new());
        assert_eq!(buf[(0, 0)].symbol(), "╭", "{out}");
        assert_eq!(buf[(39, 0)].symbol(), "╮", "{out}");
        assert_eq!(buf[(0, 2)].symbol(), "╰", "{out}");
        assert_eq!(buf[(39, 2)].symbol(), "╯", "{out}");
        assert!(out.contains("›"), "{out}");
        assert_eq!(buf[(0, 0)].style().fg, theme::border_idle().fg);
    }

    #[test]
    fn draw_keeps_slash_completion_outside_the_box() {
        let mut view = view();
        type_text(&mut view, "/");
        assert_eq!(view.overlay(), Overlay::Slash);
        let (out, _) = render(&mut view, 40, 3, &State::new());
        assert!(out.contains("/"), "{out}");
        assert!(
            !out.contains("exit") && !out.contains("End the session"),
            "slash popup must not paint inside the input box: {out}"
        );
    }

    #[test]
    fn draw_drops_border_when_height_is_one() {
        let mut view = view();
        let (out, buf) = render(&mut view, 40, 1, &State::new());
        assert_ne!(buf[(0, 0)].symbol(), "╭", "{out}");
        assert!(out.contains("›"), "{out}");
    }

    #[test]
    fn busy_does_not_change_empty_border() {
        let mut view = view();
        let busy = State {
            busy: true,
            frame: 0,
            ..State::new()
        };
        let (_, buf) = render(&mut view, 40, 3, &busy);
        assert_eq!(buf[(0, 0)].style().fg, theme::border_idle().fg);
    }

    #[test]
    fn typed_text_keeps_active_border_while_busy() {
        let mut view = view();
        type_text(&mut view, "hello");
        let busy = State {
            busy: true,
            ..State::new()
        };
        let (_, buf) = render(&mut view, 40, 3, &busy);
        assert_eq!(buf[(0, 0)].style().fg, theme::border_active().fg);
    }

    #[test]
    fn plan_mode_uses_mode_border_color() {
        let mut view = view();
        let (_, idle) = render(&mut view, 40, 3, &State::new());
        assert_eq!(idle[(0, 0)].style().fg, theme::border_idle().fg);

        let plan = State {
            mode: AgentMode::Plan,
            ..State::new()
        };
        let (_, buf) = render(&mut view, 40, 3, &plan);
        assert_eq!(buf[(0, 0)].style().fg, theme::mode().fg);

        let busy_plan = State {
            busy: true,
            mode: AgentMode::Plan,
            ..State::new()
        };
        let (_, buf) = render(&mut view, 40, 3, &busy_plan);
        assert_eq!(buf[(0, 0)].style().fg, theme::mode().fg);
    }
}

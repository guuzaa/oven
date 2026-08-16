use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::component::State;
use super::theme;

const MAX_MODEL_ROWS: usize = 6;

/// Reasoning-effort choices shown in the second stage. `keep current` submits
/// the command without an effort argument.
const EFFORT_ITEMS: [(&str, &str); 5] = [
    ("none", "Disable reasoning"),
    ("low", "Low reasoning effort"),
    ("medium", "Medium reasoning effort"),
    ("high", "High reasoning effort"),
    ("keep current", "Keep the current reasoning effort"),
];

/// Result of a key consumed by the model picker.
pub(crate) enum ModelPickerAction {
    /// The key was consumed without filling or submitting.
    Handled,
    /// Submit the composed `/model` command.
    Submit(String),
    /// Close the picker and restore the input line.
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Models,
    Effort,
}

/// Modal two-stage picker for `/model`: filter/select a model, then pick a
/// reasoning effort. Owns the keyboard while open; the input line stays frozen
/// at `/model ` and is restored to `/model` when cancelled.
pub(crate) struct ModelPicker {
    models: Vec<(String, String)>,
    pub(crate) filter: String,
    stage: Stage,
    selected: usize,
    model: Option<String>,
    open: bool,
}

impl ModelPicker {
    pub(crate) fn new(models: Vec<(String, String)>) -> Self {
        Self {
            models,
            filter: String::new(),
            stage: Stage::Models,
            selected: 0,
            model: None,
            open: false,
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Replace the model list (e.g. after a provider switch).
    pub(crate) fn update_models(&mut self, models: Vec<(String, String)>) {
        self.models = models;
        if self.stage == Stage::Models {
            self.selected = 0;
        }
    }

    /// Open the picker, seeding the filter from the typed fragment.
    pub(crate) fn open(&mut self, filter: &str) {
        self.open = true;
        self.stage = Stage::Models;
        self.filter = filter.trim().to_lowercase();
        self.model = None;
        self.selected = 0;
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.stage = Stage::Models;
        self.filter.clear();
        self.model = None;
        self.selected = 0;
    }

    /// Indices of items in the active stage: models filtered by `filter`, or
    /// the fixed effort list.
    pub(crate) fn matches(&self) -> Vec<usize> {
        match self.stage {
            Stage::Models => self
                .models
                .iter()
                .enumerate()
                .filter(|(_, (id, _))| id.to_lowercase().starts_with(&self.filter))
                .map(|(i, _)| i)
                .collect(),
            Stage::Effort => (0..EFFORT_ITEMS.len()).collect(),
        }
    }

    fn selected_item(&self) -> Option<usize> {
        self.matches().get(self.selected).copied()
    }

    /// Height of the widget, or 0 when hidden.
    pub(crate) fn height(&self, _state: &State) -> u16 {
        if !self.open {
            return 0;
        }
        match self.stage {
            Stage::Models => {
                let rows = self.matches().len().clamp(1, MAX_MODEL_ROWS);
                (rows as u16).saturating_add(1)
            }
            Stage::Effort => EFFORT_ITEMS.len() as u16,
        }
    }

    pub(crate) fn draw(&self, f: &mut Frame<'_>, area: Rect, _state: &State) {
        if !self.open {
            return;
        }
        match self.stage {
            Stage::Models => self.draw_models(f, area),
            Stage::Effort => self.draw_effort(f, area),
        }
    }

    /// Handle keys while the picker is open. Every key is consumed so the
    /// frozen input line is never edited.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ModelPickerAction {
        match self.stage {
            Stage::Models => self.handle_models_key(key),
            Stage::Effort => self.handle_effort_key(key),
        }
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if self.stage != Stage::Models {
            return;
        }
        let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if cleaned.is_empty() {
            return;
        }
        self.filter.push_str(&cleaned.to_lowercase());
        self.selected = 0;
    }

    fn handle_models_key(&mut self, key: KeyEvent) -> ModelPickerAction {
        match key.code {
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.filter.push(ch);
                self.selected = 0;
                ModelPickerAction::Handled
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.selected = 0;
                ModelPickerAction::Handled
            }
            KeyCode::Up | KeyCode::Down => {
                let n = self.matches().len();
                if n > 0 {
                    self.selected = if key.code == KeyCode::Up {
                        (self.selected + n - 1) % n
                    } else {
                        (self.selected + 1) % n
                    };
                }
                ModelPickerAction::Handled
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                if let Some(idx) = self.selected_item() {
                    self.model = Some(self.models[idx].0.clone());
                    self.stage = Stage::Effort;
                    self.selected = 0;
                }
                ModelPickerAction::Handled
            }
            KeyCode::Esc => {
                self.close();
                ModelPickerAction::Close
            }
            _ => ModelPickerAction::Handled,
        }
    }

    fn handle_effort_key(&mut self, key: KeyEvent) -> ModelPickerAction {
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                let n = EFFORT_ITEMS.len();
                self.selected = if key.code == KeyCode::Up {
                    (self.selected + n - 1) % n
                } else {
                    (self.selected + 1) % n
                };
                ModelPickerAction::Handled
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let Some(model) = self.model.clone() else {
                    return ModelPickerAction::Handled;
                };
                let (name, _) = EFFORT_ITEMS[self.selected];
                let line = if name == "keep current" {
                    format!("/model {model}")
                } else {
                    format!("/model {model} {name}")
                };
                self.close();
                ModelPickerAction::Submit(line)
            }
            KeyCode::Esc => {
                let back = self
                    .models
                    .iter()
                    .position(|(id, _)| Some(id) == self.model.as_ref())
                    .unwrap_or(0);
                self.stage = Stage::Models;
                self.model = None;
                self.filter.clear();
                self.selected = back;
                ModelPickerAction::Handled
            }
            _ => ModelPickerAction::Handled,
        }
    }

    fn draw_models(&self, f: &mut Frame<'_>, area: Rect) {
        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            Span::styled("filter: ", theme::dim()),
            Span::raw(self.filter.clone()),
        ]));
        let indices = self.matches();
        for (row, &idx) in indices.iter().take(MAX_MODEL_ROWS).enumerate() {
            let (id, provider) = &self.models[idx];
            let name_style = if row == self.selected {
                theme::accent()
            } else {
                ratatui::style::Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(id.clone(), name_style),
                Span::styled(format!("  {provider}"), theme::dim()),
            ]));
        }
        if indices.is_empty() {
            lines.push(Line::from(Span::styled("no matching models", theme::dim())));
        }
        f.render_widget(Paragraph::new(lines), area);
    }

    fn draw_effort(&self, f: &mut Frame<'_>, area: Rect) {
        let mut lines = Vec::with_capacity(EFFORT_ITEMS.len());
        for (row, (name, desc)) in EFFORT_ITEMS.iter().enumerate() {
            let name_style = if row == self.selected {
                theme::accent()
            } else {
                ratatui::style::Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(*name, name_style),
                Span::styled(format!("  {desc}"), theme::dim()),
            ]));
        }
        f.render_widget(Paragraph::new(lines), area);
    }
}

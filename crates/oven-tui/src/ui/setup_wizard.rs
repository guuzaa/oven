use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use oven_app::config::ProviderConfig;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::component::State;
use super::theme;

const KEEP: &str = "keep current";

const NAME_ITEMS: [(&str, &str); 5] = [
    ("openai", "OpenAI"),
    ("deepseek", "DeepSeek"),
    // ("moonshot", "Moonshot (Kimi)"),
    ("zhipu", "Zhipu"),
    ("grok", "Grok"),
    (KEEP, "Keep the current provider"),
];

const KIND_COMPLETIONS: (&str, &str) = ("completions", "OpenAI Chat Completions");
const KIND_RESPONSES: (&str, &str) = ("responses", "OpenAI Responses API");
const KIND_KEEP: (&str, &str) = (KEEP, "Keep the current API format");

#[derive(Debug)]
pub(crate) enum SetupWizardAction {
    Handled,
    Submit(String),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Name,
    Kind,
    ApiKey,
}

pub(crate) struct SetupWizard {
    current: ProviderConfig,
    draft: ProviderConfig,
    stage: Stage,
    selected: usize,
    buffer: String,
    open: bool,
}

impl SetupWizard {
    pub(crate) fn new(current: ProviderConfig) -> Self {
        Self {
            current,
            draft: ProviderConfig::default(),
            stage: Stage::Name,
            selected: 0,
            buffer: String::new(),
            open: false,
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn set_current(&mut self, current: ProviderConfig) {
        self.current = current;
    }

    pub(crate) fn open(&mut self) {
        self.open = true;
        self.draft = ProviderConfig::default();
        self.enter_stage(Stage::Name);
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.draft = ProviderConfig::default();
        self.stage = Stage::Name;
        self.selected = 0;
        self.buffer.clear();
    }

    pub(crate) fn height(&self, _state: &State) -> u16 {
        if !self.open {
            return 0;
        }
        match self.stage {
            Stage::Name => NAME_ITEMS.len() as u16,
            Stage::Kind => self.kind_items().len() as u16,
            Stage::ApiKey => 1,
        }
    }

    pub(crate) fn prompt_hint(&self) -> &'static str {
        match self.stage {
            Stage::Name => "choose a provider",
            Stage::Kind => "choose an API format",
            Stage::ApiKey => "",
        }
    }

    pub(crate) fn prompt_mask(&self) -> Option<String> {
        if self.stage != Stage::ApiKey {
            return None;
        }
        Some("*".repeat(self.buffer.chars().count()))
    }

    pub(crate) fn draw(&self, f: &mut Frame<'_>, area: Rect, _state: &State) {
        if !self.open {
            return;
        }
        match self.stage {
            Stage::Name => self.draw_list(f, area, &NAME_ITEMS),
            Stage::Kind => self.draw_list(f, area, self.kind_items()),
            Stage::ApiKey => {
                let label = if self.requires_new_key() {
                    "api_key · required · Enter to save · Esc to go back"
                } else {
                    "api_key · empty keeps current · Enter to save · Esc to go back"
                };
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(label, theme::dim()))),
                    area,
                );
            }
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> SetupWizardAction {
        match self.stage {
            Stage::Name => self.handle_name_key(key),
            Stage::Kind => self.handle_kind_key(key),
            Stage::ApiKey => self.handle_api_key(key),
        }
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if self.stage != Stage::ApiKey {
            return;
        }
        let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        self.buffer.push_str(&cleaned);
    }

    fn enter_stage(&mut self, stage: Stage) {
        self.stage = stage;
        self.selected = 0;
        self.buffer.clear();
        match stage {
            Stage::Name => self.selected = preselect_name(&self.current),
            Stage::Kind => self.selected = self.preselect_kind(),
            Stage::ApiKey => {}
        }
    }

    fn handle_name_key(&mut self, key: KeyEvent) -> SetupWizardAction {
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                cycle_selected(
                    &mut self.selected,
                    NAME_ITEMS.len(),
                    key.code == KeyCode::Up,
                );
                SetupWizardAction::Handled
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let id = NAME_ITEMS[self.selected].0;
                self.draft.name = if id == KEEP {
                    None
                } else {
                    Some(id.to_string())
                };
                self.enter_stage(Stage::Kind);
                SetupWizardAction::Handled
            }
            KeyCode::Esc => {
                self.close();
                SetupWizardAction::Close
            }
            _ => SetupWizardAction::Handled,
        }
    }

    fn handle_kind_key(&mut self, key: KeyEvent) -> SetupWizardAction {
        let items = self.kind_items();
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                cycle_selected(&mut self.selected, items.len(), key.code == KeyCode::Up);
                SetupWizardAction::Handled
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let id = items[self.selected].0;
                self.draft.kind = if id == KEEP {
                    None
                } else {
                    ProviderConfig::parse_kind(id)
                };
                self.enter_stage(Stage::ApiKey);
                SetupWizardAction::Handled
            }
            KeyCode::Esc => {
                self.enter_stage(Stage::Name);
                SetupWizardAction::Handled
            }
            _ => SetupWizardAction::Handled,
        }
    }

    fn handle_api_key(&mut self, key: KeyEvent) -> SetupWizardAction {
        if let Some(ch) = typing_char(key) {
            self.buffer.push(ch);
            return SetupWizardAction::Handled;
        }
        match key.code {
            KeyCode::Backspace => {
                self.buffer.pop();
                SetupWizardAction::Handled
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let value = self.buffer.trim();
                if value.is_empty() && self.requires_new_key() {
                    return SetupWizardAction::Handled;
                }
                self.draft.api_key = nonempty(value);
                match compose(&self.draft) {
                    Some(line) => {
                        self.close();
                        SetupWizardAction::Submit(line)
                    }
                    None => {
                        self.close();
                        SetupWizardAction::Close
                    }
                }
            }
            KeyCode::Esc => {
                self.enter_stage(Stage::Kind);
                SetupWizardAction::Handled
            }
            _ => SetupWizardAction::Handled,
        }
    }

    fn kind_items(&self) -> &'static [(&'static str, &'static str)] {
        kinds_for(self.draft.name.as_deref())
    }

    fn preselect_kind(&self) -> usize {
        let items = self.kind_items();
        if let Some(k) = self.current.kind {
            let slug = k.to_string();
            if let Some(i) = items.iter().position(|(id, _)| *id == slug) {
                return i;
            }
        }
        items
            .iter()
            .position(|(id, _)| *id != KEEP)
            .unwrap_or(items.len().saturating_sub(1))
    }

    fn requires_new_key(&self) -> bool {
        match self.draft.name.as_deref() {
            Some(name) => self.current.name.as_deref() != Some(name),
            None => false,
        }
    }

    fn draw_list(&self, f: &mut Frame<'_>, area: Rect, items: &[(&str, &str)]) {
        let mut lines = Vec::with_capacity(items.len());
        for (row, (name, desc)) in items.iter().enumerate() {
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

fn compose(draft: &ProviderConfig) -> Option<String> {
    let mut parts = vec!["/setup".to_string()];
    if let Some(n) = &draft.name {
        parts.push(format!("name={n}"));
    }
    if let Some(k) = draft.kind {
        parts.push(format!("kind={k}"));
    }
    if let Some(k) = &draft.api_key {
        parts.push(format!("api_key={k}"));
    }
    if parts.len() == 1 {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn nonempty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn cycle_selected(selected: &mut usize, n: usize, up: bool) {
    if n == 0 {
        return;
    }
    *selected = if up {
        (*selected + n - 1) % n
    } else {
        (*selected + 1) % n
    };
}

fn kinds_for(name: Option<&str>) -> &'static [(&'static str, &'static str)] {
    match name {
        Some("openai") | Some("deepseek") => &[KIND_COMPLETIONS, KIND_RESPONSES],
        Some("moonshot") | Some("zhipu") => &[KIND_COMPLETIONS],
        Some("grok") => &[KIND_RESPONSES],
        _ => &[KIND_COMPLETIONS, KIND_RESPONSES, KIND_KEEP],
    }
}

fn preselect_name(current: &ProviderConfig) -> usize {
    let Some(slug) = current
        .name
        .as_deref()
        .map(str::to_ascii_lowercase)
        .filter(|s| !s.is_empty())
    else {
        return NAME_ITEMS.len() - 1;
    };
    let slug = match slug.as_str() {
        "kimi" => "moonshot",
        "glm" => "zhipu",
        other => other,
    };
    NAME_ITEMS
        .iter()
        .position(|(id, _)| *id == slug)
        .unwrap_or(NAME_ITEMS.len() - 1)
}

fn typing_char(key: KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(ch)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            Some(ch)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use oven_llm::ProviderKind;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn open() -> SetupWizard {
        let mut w = SetupWizard::new(ProviderConfig::default());
        w.open();
        w
    }

    #[test]
    fn enter_on_deepseek_then_completions_composes_command() {
        let mut w = SetupWizard::new(ProviderConfig {
            name: Some("deepseek".into()),
            ..Default::default()
        });
        w.open();
        assert!(matches!(
            w.handle_key(key(KeyCode::Enter)),
            SetupWizardAction::Handled
        ));
        assert_eq!(w.stage, Stage::Kind);
        assert!(matches!(
            w.handle_key(key(KeyCode::Enter)),
            SetupWizardAction::Handled
        ));
        assert_eq!(w.stage, Stage::ApiKey);
        for ch in "sk-secret".chars() {
            w.handle_key(key(KeyCode::Char(ch)));
        }
        match w.handle_key(key(KeyCode::Enter)) {
            SetupWizardAction::Submit(line) => {
                assert_eq!(
                    line,
                    "/setup name=deepseek kind=completions api_key=sk-secret"
                );
            }
            other => panic!("expected Submit, got {other:?}"),
        }
        assert!(!w.is_open());
    }

    #[test]
    fn keep_current_with_empty_key_closes() {
        let mut w = SetupWizard::new(ProviderConfig {
            name: Some("openai".into()),
            kind: Some(ProviderKind::Responses),
            model: Some("gpt-4o".into()),
            ..Default::default()
        });
        w.open();
        for _ in 0..NAME_ITEMS.len() - 1 {
            w.handle_key(key(KeyCode::Down));
        }
        w.handle_key(key(KeyCode::Enter));
        w.handle_key(key(KeyCode::Down));
        w.handle_key(key(KeyCode::Enter));
        match w.handle_key(key(KeyCode::Enter)) {
            SetupWizardAction::Close => {}
            other => panic!("expected Close, got {other:?}"),
        }
        assert!(!w.is_open());
    }

    #[test]
    fn keep_current_only_sends_api_key() {
        let mut w = SetupWizard::new(ProviderConfig {
            name: Some("openai".into()),
            kind: Some(ProviderKind::Responses),
            ..Default::default()
        });
        w.open();
        for _ in 0..NAME_ITEMS.len() - 1 {
            w.handle_key(key(KeyCode::Down));
        }
        w.handle_key(key(KeyCode::Enter));
        w.handle_key(key(KeyCode::Down));
        w.handle_key(key(KeyCode::Enter));
        for ch in "sk-new".chars() {
            w.handle_key(key(KeyCode::Char(ch)));
        }
        match w.handle_key(key(KeyCode::Enter)) {
            SetupWizardAction::Submit(line) => {
                assert_eq!(line, "/setup api_key=sk-new");
            }
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn switching_provider_requires_api_key() {
        let mut w = SetupWizard::new(ProviderConfig {
            name: Some("deepseek".into()),
            api_key: Some("sk-old".into()),
            ..Default::default()
        });
        w.open();
        w.handle_key(key(KeyCode::Down));
        w.handle_key(key(KeyCode::Enter));
        w.handle_key(key(KeyCode::Enter));
        assert_eq!(w.stage, Stage::ApiKey);
        assert!(matches!(
            w.handle_key(key(KeyCode::Enter)),
            SetupWizardAction::Handled
        ));
        assert_eq!(w.stage, Stage::ApiKey);
        assert!(w.is_open());
        for ch in "sk-new".chars() {
            w.handle_key(key(KeyCode::Char(ch)));
        }
        match w.handle_key(key(KeyCode::Enter)) {
            SetupWizardAction::Submit(line) => {
                assert_eq!(line, "/setup name=zhipu kind=completions api_key=sk-new");
            }
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn grok_only_offers_responses() {
        let mut w = SetupWizard::new(ProviderConfig {
            name: Some("grok".into()),
            ..Default::default()
        });
        w.open();
        w.handle_key(key(KeyCode::Enter));
        assert_eq!(w.stage, Stage::Kind);
        assert_eq!(w.kind_items(), &[KIND_RESPONSES]);
        w.handle_key(key(KeyCode::Enter));
        for ch in "xai-key".chars() {
            w.handle_key(key(KeyCode::Char(ch)));
        }
        match w.handle_key(key(KeyCode::Enter)) {
            SetupWizardAction::Submit(line) => {
                assert_eq!(line, "/setup name=grok kind=responses api_key=xai-key");
            }
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn esc_on_first_stage_closes() {
        let mut w = open();
        assert!(matches!(
            w.handle_key(key(KeyCode::Esc)),
            SetupWizardAction::Close
        ));
        assert!(!w.is_open());
    }

    #[test]
    fn esc_on_kind_returns_to_name() {
        let mut w = open();
        w.handle_key(key(KeyCode::Enter));
        assert_eq!(w.stage, Stage::Kind);
        w.handle_key(key(KeyCode::Esc));
        assert_eq!(w.stage, Stage::Name);
        assert!(w.is_open());
    }

    #[test]
    fn api_key_is_masked_in_buffer_draw_state() {
        let mut w = open();
        w.enter_stage(Stage::ApiKey);
        for ch in "abcd".chars() {
            w.handle_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(w.buffer, "abcd");
        assert_eq!(w.prompt_mask().as_deref(), Some("****"));
        assert_eq!(w.prompt_hint(), "");
    }

    #[test]
    fn list_stages_show_hint_not_mask() {
        let w = open();
        assert_eq!(w.prompt_hint(), "choose a provider");
        assert!(w.prompt_mask().is_none());
    }

    #[test]
    fn shift_uppercase_chars_are_kept_in_api_key() {
        let mut w = open();
        w.enter_stage(Stage::ApiKey);
        for (ch, mods) in [
            ('s', KeyModifiers::NONE),
            ('k', KeyModifiers::NONE),
            ('-', KeyModifiers::NONE),
            ('6', KeyModifiers::NONE),
            ('T', KeyModifiers::SHIFT),
            ('B', KeyModifiers::SHIFT),
            ('p', KeyModifiers::NONE),
        ] {
            w.handle_key(KeyEvent::new(KeyCode::Char(ch), mods));
        }
        assert_eq!(w.buffer, "sk-6TBp");
    }

    #[test]
    fn paste_strips_whitespace_from_api_key() {
        let mut w = open();
        w.enter_stage(Stage::ApiKey);
        w.paste("sk-6TBpKsAJ\nQuU31N8 ");
        assert_eq!(w.buffer, "sk-6TBpKsAJQuU31N8");
    }
}

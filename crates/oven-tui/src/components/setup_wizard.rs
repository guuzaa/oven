use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use oven_app::config::ProviderConfig;
use oven_llm::canonical_vendor;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::list;
use super::theme;

const KEEP: &str = "keep current";

const NAME_ITEMS: [(&str, &str); 6] = [
    ("openai", "OpenAI"),
    ("deepseek", "DeepSeek"),
    ("zhipu", "Zhipu (GLM)"),
    // ("moonshot", "Moonshot (Kimi)"),
    ("xai", "Grok"),
    ("custom", "Custom gateway"),
    (KEEP, "Keep the current provider"),
];

const PROTOCOL_ITEMS: [(&str, &str); 2] = [
    ("completions", "OpenAI Chat Completions"),
    ("responses", "OpenAI Responses API"),
];

#[derive(Debug)]
pub(crate) enum SetupWizardAction {
    Handled,
    Submit(String),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Name,
    CustomName,
    BaseUrl,
    Protocol,
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

    pub(crate) fn height(&self) -> u16 {
        if !self.open {
            return 0;
        }
        match self.stage {
            Stage::Name => NAME_ITEMS.len() as u16,
            Stage::Protocol => PROTOCOL_ITEMS.len() as u16,
            Stage::CustomName | Stage::BaseUrl | Stage::ApiKey => 1,
        }
    }

    pub(crate) fn prompt_hint(&self) -> &'static str {
        match self.stage {
            Stage::Name => "choose a provider",
            Stage::CustomName => "",
            Stage::BaseUrl => "",
            Stage::Protocol => "choose an API protocol",
            Stage::ApiKey => "",
        }
    }

    pub(crate) fn prompt_value(&self) -> Option<String> {
        match self.stage {
            Stage::CustomName | Stage::BaseUrl => Some(self.buffer.clone()),
            Stage::ApiKey => Some("*".repeat(self.buffer.chars().count())),
            _ => None,
        }
    }

    pub(crate) fn draw(&self, f: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        match self.stage {
            Stage::Name => self.draw_list(f, area, &NAME_ITEMS),
            Stage::Protocol => self.draw_list(f, area, &PROTOCOL_ITEMS),
            Stage::CustomName => self.draw_label(
                f,
                area,
                "name · gateway id (e.g. my-proxy) · Enter to continue · Esc to go back",
            ),
            Stage::BaseUrl => self.draw_label(
                f,
                area,
                "base_url · required · Enter to continue · Esc to go back",
            ),
            Stage::ApiKey => {
                let label = if self.requires_new_key() {
                    "api_key · required · Enter to save · Esc to go back"
                } else {
                    "api_key · empty keeps current · Enter to save · Esc to go back"
                };
                self.draw_label(f, area, label);
            }
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> SetupWizardAction {
        match self.stage {
            Stage::Name => self.handle_name_key(key),
            Stage::CustomName => self.handle_text_stage(key, Stage::CustomName),
            Stage::BaseUrl => self.handle_text_stage(key, Stage::BaseUrl),
            Stage::Protocol => self.handle_protocol_key(key),
            Stage::ApiKey => self.handle_api_key(key),
        }
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if !matches!(
            self.stage,
            Stage::ApiKey | Stage::CustomName | Stage::BaseUrl
        ) {
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
            Stage::Protocol => {}
            Stage::CustomName | Stage::BaseUrl | Stage::ApiKey => {}
        }
    }

    fn handle_name_key(&mut self, key: KeyEvent) -> SetupWizardAction {
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                list::cycle_selected(
                    &mut self.selected,
                    NAME_ITEMS.len(),
                    key.code == KeyCode::Up,
                );
                SetupWizardAction::Handled
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let id = NAME_ITEMS[self.selected].0;
                if id == KEEP {
                    self.draft.name = None;
                    self.enter_stage(Stage::ApiKey);
                } else if id == "custom" {
                    self.enter_stage(Stage::CustomName);
                } else {
                    self.draft.name = Some(id.to_string());
                    self.enter_stage(Stage::ApiKey);
                }
                SetupWizardAction::Handled
            }
            KeyCode::Esc => {
                self.close();
                SetupWizardAction::Close
            }
            _ => SetupWizardAction::Handled,
        }
    }

    fn handle_text_stage(&mut self, key: KeyEvent, stage: Stage) -> SetupWizardAction {
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
                if value.is_empty() {
                    return SetupWizardAction::Handled;
                }
                match stage {
                    Stage::CustomName => {
                        self.draft.name = Some(canonical_vendor(value));
                        self.enter_stage(Stage::BaseUrl);
                    }
                    Stage::BaseUrl => {
                        self.draft.base_url = Some(value.to_string());
                        self.enter_stage(Stage::Protocol);
                    }
                    _ => {}
                }
                SetupWizardAction::Handled
            }
            KeyCode::Esc => {
                let back = match stage {
                    Stage::CustomName => Stage::Name,
                    Stage::BaseUrl => Stage::CustomName,
                    _ => Stage::Name,
                };
                self.enter_stage(back);
                SetupWizardAction::Handled
            }
            _ => SetupWizardAction::Handled,
        }
    }

    fn handle_protocol_key(&mut self, key: KeyEvent) -> SetupWizardAction {
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                list::cycle_selected(
                    &mut self.selected,
                    PROTOCOL_ITEMS.len(),
                    key.code == KeyCode::Up,
                );
                SetupWizardAction::Handled
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                self.draft.protocol =
                    ProviderConfig::parse_protocol(PROTOCOL_ITEMS[self.selected].0);
                self.enter_stage(Stage::ApiKey);
                SetupWizardAction::Handled
            }
            KeyCode::Esc => {
                self.enter_stage(Stage::BaseUrl);
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
                if self.draft.base_url.is_some() {
                    self.enter_stage(Stage::Protocol);
                } else {
                    self.enter_stage(Stage::Name);
                }
                SetupWizardAction::Handled
            }
            _ => SetupWizardAction::Handled,
        }
    }

    fn requires_new_key(&self) -> bool {
        match self.draft.name.as_deref() {
            Some(name) => self.current.name.as_deref() != Some(name),
            None => false,
        }
    }

    fn draw_list(&self, f: &mut Frame<'_>, area: Rect, items: &[(&str, &str)]) {
        list::draw_choice_list(f, area, items.iter().copied(), self.selected);
    }

    fn draw_label(&self, f: &mut Frame<'_>, area: Rect, label: &str) {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(label, theme::dim()))),
            area,
        );
    }
}

fn compose(draft: &ProviderConfig) -> Option<String> {
    let mut parts = vec!["/setup".to_string()];
    if let Some(n) = &draft.name {
        parts.push(format!("name={n}"));
    }
    if let Some(u) = &draft.base_url {
        parts.push(format!("base_url={u}"));
    }
    if let Some(p) = draft.protocol {
        parts.push(format!("protocol={p}"));
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

fn preselect_name(current: &ProviderConfig) -> usize {
    let Some(slug) = current
        .name
        .as_deref()
        .map(canonical_vendor)
        .filter(|s| !s.is_empty())
    else {
        return NAME_ITEMS.len() - 1;
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn open() -> SetupWizard {
        let mut w = SetupWizard::new(ProviderConfig::default());
        w.open();
        w
    }

    #[test]
    fn enter_on_deepseek_composes_command_without_kind() {
        let mut w = SetupWizard::new(ProviderConfig {
            name: Some("deepseek".into()),
            ..Default::default()
        });
        w.open();
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
                assert_eq!(line, "/setup name=deepseek api_key=sk-secret");
            }
            other => panic!("expected Submit, got {other:?}"),
        }
        assert!(!w.is_open());
    }

    #[test]
    fn keep_current_with_empty_key_closes() {
        let mut w = SetupWizard::new(ProviderConfig {
            name: Some("openai".into()),
            model: Some("openai/gpt-4o".into()),
            ..Default::default()
        });
        w.open();
        for _ in 0..NAME_ITEMS.len() - 1 {
            w.handle_key(key(KeyCode::Down));
        }
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
            ..Default::default()
        });
        w.open();
        for _ in 0..NAME_ITEMS.len() - 1 {
            w.handle_key(key(KeyCode::Down));
        }
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
                assert_eq!(line, "/setup name=zhipu api_key=sk-new");
            }
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn grok_writes_canonical_xai() {
        let mut w = SetupWizard::new(ProviderConfig {
            name: Some("grok".into()),
            ..Default::default()
        });
        w.open();
        assert_eq!(NAME_ITEMS[w.selected].0, "xai");
        w.handle_key(key(KeyCode::Enter));
        assert_eq!(w.stage, Stage::ApiKey);
        for ch in "xai-key".chars() {
            w.handle_key(key(KeyCode::Char(ch)));
        }
        match w.handle_key(key(KeyCode::Enter)) {
            SetupWizardAction::Submit(line) => {
                assert_eq!(line, "/setup name=xai api_key=xai-key");
            }
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn custom_asks_for_base_url_and_protocol() {
        let mut w = open();
        w.selected = NAME_ITEMS
            .iter()
            .position(|(id, _)| *id == "custom")
            .unwrap();
        w.handle_key(key(KeyCode::Enter));
        assert_eq!(w.stage, Stage::CustomName);
        for ch in "my-proxy".chars() {
            w.handle_key(key(KeyCode::Char(ch)));
        }
        w.handle_key(key(KeyCode::Enter));
        assert_eq!(w.stage, Stage::BaseUrl);
        for ch in "https://proxy.example/v1".chars() {
            w.handle_key(key(KeyCode::Char(ch)));
        }
        w.handle_key(key(KeyCode::Enter));
        assert_eq!(w.stage, Stage::Protocol);
        w.handle_key(key(KeyCode::Enter));
        assert_eq!(w.stage, Stage::ApiKey);
        for ch in "sk-gw".chars() {
            w.handle_key(key(KeyCode::Char(ch)));
        }
        match w.handle_key(key(KeyCode::Enter)) {
            SetupWizardAction::Submit(line) => {
                assert_eq!(
                    line,
                    "/setup name=my-proxy base_url=https://proxy.example/v1 protocol=completions api_key=sk-gw"
                );
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
    fn esc_on_api_key_returns_to_name() {
        let mut w = open();
        w.handle_key(key(KeyCode::Enter));
        assert_eq!(w.stage, Stage::ApiKey);
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
        assert_eq!(w.prompt_value().as_deref(), Some("****"));
        assert_eq!(w.prompt_hint(), "");
    }

    #[test]
    fn custom_name_and_base_url_echo_typed_text() {
        let mut w = open();
        w.enter_stage(Stage::CustomName);
        assert_eq!(w.prompt_value().as_deref(), Some(""));
        for ch in "my-proxy".chars() {
            w.handle_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(w.prompt_value().as_deref(), Some("my-proxy"));

        w.handle_key(key(KeyCode::Enter));
        assert_eq!(w.stage, Stage::BaseUrl);
        assert_eq!(w.prompt_value().as_deref(), Some(""));
        for ch in "https://proxy.example/v1".chars() {
            w.handle_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(
            w.prompt_value().as_deref(),
            Some("https://proxy.example/v1")
        );
        assert_eq!(w.prompt_hint(), "");
    }

    #[test]
    fn list_stages_show_hint_not_mask() {
        let w = open();
        assert_eq!(w.prompt_hint(), "choose a provider");
        assert!(w.prompt_value().is_none());
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

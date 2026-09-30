use std::path::{Component as PathComponent, Path};
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use oven_app::{
    AgentEvent, AgentMode, AppEvent, AppEventKind, AppState, CompactionEvent, TurnEvent,
    context_tokens_of,
};
use oven_llm::{ReasoningEffort, Usage};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::core::component::{Component, KeyResult, State};
use crate::core::theme;

const SPIN_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const REPLY_TTL: Duration = Duration::from_secs(3);
const REPLY_FLASH: Duration = Duration::from_millis(150);
const SEP: &str = " · ";
const TOAST_MAX_WIDTH: u16 = 60;
const TOAST_MAX_TEXT_ROWS: u16 = 8;
const FRAME_SPAN: u16 = 2;
const SPIN_COLS: usize = 2;
const COMPACTING: &str = "compacting…";

/// One row below the input: `[spin] model [effort] · mode · root · ctx% · usage`.
/// Segments are dropped from the tail when the row is too narrow to hold them
/// all, so no number is ever shown half cut.
pub struct StatusBar {
    model: String,
    effort: Option<ReasoningEffort>,
    root: String,
    usage: Usage,
    context_tokens: u32,
    context_window: Option<u32>,
    compacting: bool,
    reply: Option<String>,
    reply_until: Option<Instant>,
    flash_until: Option<Instant>,
}

/// A rendered piece of the status row: text plus the style it carries.
type Segment = (String, Style);

impl StatusBar {
    pub fn new(model: impl Into<String>, root: &Path, usage: Usage) -> Self {
        Self {
            model: model.into(),
            effort: None,
            root: short_root(root),
            usage,
            context_tokens: 0,
            context_window: None,
            compacting: false,
            reply: None,
            reply_until: None,
            flash_until: None,
        }
    }

    pub fn with_effort(mut self, effort: Option<ReasoningEffort>) -> Self {
        self.effort = effort;
        self
    }

    pub fn with_context(mut self, tokens: u32, window: Option<u32>) -> Self {
        self.context_tokens = tokens;
        self.context_window = window;
        self
    }

    /// Follows the app's state. The usage readout is only taken between
    /// turns: while one runs it reports its own usage as it goes, and the
    /// snapshot lags behind until the turn ends.
    pub fn sync(&mut self, state: &AppState) {
        self.model.clone_from(&state.model);
        self.effort = state.reasoning_effort;
        self.context_window = state.context_window;
        if state.phase.is_idle() {
            self.usage = state.last_turn_usage;
            self.context_tokens = state.context_tokens;
        }
    }

    pub fn has_reply(&self) -> bool {
        self.reply.as_ref().is_some_and(|t| !t.is_empty())
    }

    pub fn expire_reply(&mut self) -> bool {
        let flash_done = self
            .flash_until
            .is_some_and(|until| Instant::now() >= until);
        if flash_done {
            self.flash_until = None;
        }
        match self.reply_until {
            Some(until) if Instant::now() >= until => {
                self.clear_reply();
                true
            }
            _ => flash_done,
        }
    }

    /// Bottom-right toast over `area`, answering a slash command's reply. A
    /// flashing toast clears first and paints nothing, so a repeated reply
    /// blinks once before it is read.
    pub fn draw_reply_overlay(&self, f: &mut Frame<'_>, area: Rect) {
        let Some(text) = self.reply.as_deref().filter(|t| !t.is_empty()) else {
            return;
        };
        if area.width < 8 || area.height < 3 {
            return;
        }

        let width = area.width.min(TOAST_MAX_WIDTH);
        let (rows, text_width) = wrapped_rows(text, width.saturating_sub(FRAME_SPAN));
        if rows == 0 {
            return;
        }
        let width = text_width.saturating_add(FRAME_SPAN).min(width);
        let height = u16::try_from(rows)
            .unwrap_or(TOAST_MAX_TEXT_ROWS)
            .saturating_add(FRAME_SPAN)
            .min(area.height);
        let toast = Rect {
            x: area.right().saturating_sub(width + 1),
            // Anchored below its own bottom row: one higher and the toast
            // would cover whatever sits on top of the anchor.
            y: area.bottom().saturating_sub(height + 1).max(area.y),
            width,
            height,
        };

        f.render_widget(Clear, toast);
        if self.is_flashing() {
            return;
        }
        f.render_widget(
            Paragraph::new(Span::styled(text, theme::reply()))
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(theme::border_type())
                        .border_style(theme::reply()),
                ),
            toast,
        );
    }

    pub fn draw_bar(&self, f: &mut Frame<'_>, area: Rect, state: &State) {
        let spin = state.busy.then(|| spin_frame(state.frame));
        let spare =
            usize::from(area.width).saturating_sub(if spin.is_some() { SPIN_COLS } else { 0 });
        let mut spans = Vec::new();
        if let Some(frame) = spin {
            spans.push(Span::styled(frame.to_string(), theme::accent()));
            spans.push(Span::raw(" "));
        }
        for (i, (text, style)) in fit(self.segments(state.mode), spare).iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(SEP));
            }
            spans.push(Span::styled(text.clone(), *style));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    pub(crate) fn clear_reply(&mut self) {
        self.reply = None;
        self.reply_until = None;
        self.flash_until = None;
    }

    fn set_reply(&mut self, text: String) {
        let flash = self.has_reply();
        self.reply = Some(text);
        self.reply_until = Some(Instant::now() + REPLY_TTL);
        self.flash_until = flash.then(|| Instant::now() + REPLY_FLASH);
    }

    fn is_flashing(&self) -> bool {
        self.flash_until.is_some_and(|until| Instant::now() < until)
    }

    /// The row's segments, most important first.
    fn segments(&self, mode: AgentMode) -> Vec<Segment> {
        let gray = theme::dim();
        let mut segments = vec![
            (self.model_label(), theme::model()),
            (mode.label().to_string(), mode_style(mode, gray)),
            (self.root.clone(), theme::path()),
        ];
        segments.extend(self.context_segment(gray));
        segments
    }

    fn model_label(&self) -> String {
        match self.effort {
            Some(effort) => format!("{} {effort}", self.model),
            None => self.model.clone(),
        }
    }

    /// Context occupancy plus the share of the prompt the KV cache served; a
    /// running compaction reports itself instead.
    fn context_segment(&self, gray: Style) -> Option<Segment> {
        if self.compacting {
            return Some((COMPACTING.to_string(), theme::accent()));
        }
        let context = self.context_label()?;
        let label = match cache_hit_percent(&self.usage) {
            Some(hit) => format!("{context} · cache {hit}%"),
            None => context,
        };
        Some((label, gray))
    }

    /// Share of the context window in use, or — when the window is unknown,
    /// as with hand-configured providers — the prompt size itself.
    fn context_label(&self) -> Option<String> {
        if let Some(pct) = context_percent(self.context_tokens, self.context_window) {
            return Some(format!("ctx {pct}%"));
        }
        (self.context_tokens > 0).then(|| format!("ctx {}", human(self.context_tokens)))
    }
}

impl Component for StatusBar {
    fn handle_key(&mut self, _key: KeyEvent, _state: &State) -> KeyResult {
        KeyResult::Ignored
    }

    fn on_event(&mut self, ev: &AppEvent) {
        match &ev.kind {
            AppEventKind::Agent(env) => match &env.event {
                AgentEvent::Turn(TurnEvent::Completed { usage, .. })
                | AgentEvent::Usage { usage } => {
                    self.usage = *usage;
                    self.context_tokens = context_tokens_of(usage);
                }
                _ => {}
            },
            AppEventKind::HistoryChanged { .. } => self.clear_reply(),
            AppEventKind::Compaction(event) => {
                self.compacting = matches!(event, CompactionEvent::Started);
            }
            AppEventKind::Notification { text } => {
                self.set_reply(text.clone());
            }
            _ => {}
        }
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect, state: &State) {
        self.draw_bar(f, area, state);
    }
}

/// Rows `text` occupies once wrapped to `width`, rendered into a scratch
/// buffer so the box is exactly as tall as its content. Also returns the
/// widest of those rows, so the box can close in around the text.
fn wrapped_rows(text: &str, width: u16) -> (usize, u16) {
    let area = Rect::new(0, 0, width, TOAST_MAX_TEXT_ROWS);
    let mut buf = Buffer::empty(area);
    Paragraph::new(text)
        .wrap(Wrap { trim: true })
        .render(area, &mut buf);
    let mut rows = 0;
    let mut widest = 0;
    for y in 0..TOAST_MAX_TEXT_ROWS {
        let row: String = (0..width).map(|x| buf[(x, y)].symbol()).collect();
        let row = row.trim_end();
        if row.is_empty() {
            continue;
        }
        rows += 1;
        widest = widest.max(u16::try_from(row.width()).unwrap_or(width));
    }
    (rows, widest)
}

fn spin_frame(frame: u64) -> char {
    SPIN_FRAMES[usize::try_from(frame).unwrap_or(0) % SPIN_FRAMES.len()]
}

/// Drops the lowest-priority tail until the row fits `width`; the segment that
/// survives alone is never sliced in half.
fn fit(mut segments: Vec<Segment>, width: usize) -> Vec<Segment> {
    while segments.len() > 1 && segments_width(&segments) > width {
        segments.pop();
    }
    segments
}

fn segments_width(segments: &[Segment]) -> usize {
    let text: usize = segments.iter().map(|(text, _)| text.width()).sum();
    text + SEP.width() * segments.len().saturating_sub(1)
}

fn mode_style(mode: AgentMode, gray: Style) -> Style {
    match mode {
        AgentMode::Agent => gray,
        AgentMode::Plan => theme::mode(),
        AgentMode::Ask => theme::ask_mode(),
    }
}

/// The last two path components: enough to tell concurrent sessions apart,
/// short enough to share the row with the rest of the state.
fn short_root(path: &Path) -> String {
    let names: Vec<String> = path
        .components()
        .filter_map(|part| match part {
            PathComponent::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    match names.as_slice() {
        [] => path.display().to_string(),
        [only] => only.clone(),
        [.., parent, last] => format!("{parent}/{last}"),
    }
}

/// Percentage of the context window in use; `None` hides the segment (no
/// window known, or nothing measured yet).
fn context_percent(tokens: u32, window: Option<u32>) -> Option<u32> {
    let window = window.filter(|w| *w > 0)?;
    (tokens > 0).then(|| u32::try_from(u64::from(tokens) * 100 / u64::from(window)).unwrap_or(0))
}

/// Share of the last turn's prompt-side tokens (`input + cache reads`, the
/// accounting the app itself uses) that the cache answered; `None` while no
/// prompt-side token has been recorded.
fn cache_hit_percent(u: &Usage) -> Option<u32> {
    let prompt = u.input_tokens.saturating_add(u.cache_read_tokens);
    (prompt > 0).then(|| {
        u32::try_from(u64::from(u.cache_read_tokens) * 100 / u64::from(prompt)).unwrap_or(0)
    })
}

fn human(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", f64::from(n) / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", f64::from(n) / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use oven_app::{AppPhase, TurnId};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    use super::*;
    use crate::core::component::idle_state;

    const MODEL: &str = "deepseek-chat";
    const ROOT: &str = "rust/oven";
    const CTX: &str = "ctx 42% · cache 39%";
    /// The same bar with no context window known: the count stands in for the
    /// share.
    const COUNTED: &str = "ctx 2.0k · cache 39%";
    const SHORT_REPLY: &str = "current model: gpt-4o";

    fn usage() -> Usage {
        Usage {
            input_tokens: 1200,
            output_tokens: 56,
            cache_read_tokens: 789,
            reasoning_tokens: 10,
        }
    }

    fn bar() -> StatusBar {
        StatusBar::new(MODEL, Path::new("/home/code/rust/oven"), usage())
            .with_effort(Some(ReasoningEffort::High))
    }

    fn agent_event(event: AgentEvent) -> AppEvent {
        AppEvent::agent(event)
    }

    fn row(width: u16, bar: &StatusBar, state: &State) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        terminal.draw(|f| bar.draw_bar(f, f.area(), state)).unwrap();
        (0..width)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol().to_string())
            .collect()
    }

    fn toast_buffer(bar: &StatusBar, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| bar.draw_reply_overlay(f, f.area()))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
        buf.content().iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn full_width_row_carries_every_segment() {
        let bar = bar().with_context(42_000, Some(100_000));
        let state = State::new();
        let rendered = row(120, &bar, &state);
        assert!(rendered.starts_with(&format!("{MODEL} high · agent · {ROOT} · {CTX}")));
    }

    #[test]
    fn busy_row_leads_with_a_spinner() {
        let bar = bar();
        let mut state = State::new();
        state.busy = true;
        let rendered = row(120, &bar, &state);
        assert!(rendered.starts_with(SPIN_FRAMES[0]), "{rendered}");
    }

    #[test]
    fn narrow_row_drops_the_tail_before_cutting_numbers() {
        let bar = bar().with_context(42_000, Some(100_000));
        let state = State::new();
        let rendered = row(43, &bar, &state);
        assert!(rendered.starts_with(&format!("{MODEL} high · agent · {ROOT}")));
        assert!(!rendered.contains(CTX), "{rendered}");
        assert!(!rendered.contains("…"), "{rendered}");

        let tight = row(31, &bar, &state);
        assert!(
            tight.starts_with(&format!("{MODEL} high · agent")),
            "{tight}"
        );
        assert!(!tight.contains(ROOT), "{tight}");
    }

    #[test]
    fn the_row_reports_ratios_not_token_counts() {
        let bar = bar().with_context(42_000, Some(100_000));
        let rendered = row(80, &bar, &State::new());
        assert!(rendered.contains(CTX), "{rendered}");
        assert!(!rendered.contains(" in "), "{rendered}");
        assert!(!rendered.contains(" out "), "{rendered}");
        assert!(!rendered.contains("789"), "{rendered}");

        let unwalked = StatusBar::new(MODEL, Path::new("/tmp"), Usage::default())
            .with_context(42_000, Some(100_000));
        let rendered = row(80, &unwalked, &State::new());
        assert!(rendered.contains("ctx 42%"), "{rendered}");
        assert!(!rendered.contains("cache"), "{rendered}");
    }

    #[test]
    fn an_unknown_window_falls_back_to_the_prompt_size() {
        let bar = bar().with_context(1989, None);
        let rendered = row(80, &bar, &State::new());
        assert!(
            rendered.starts_with(&format!("{MODEL} high · agent · {ROOT} · {COUNTED}")),
            "{rendered}"
        );

        let nothing = StatusBar::new(MODEL, Path::new("/tmp"), Usage::default());
        let rendered = row(80, &nothing, &State::new());
        assert!(!rendered.contains("ctx"), "{rendered}");
    }

    #[test]
    fn cache_hit_is_the_share_of_prompt_tokens_read_from_cache() {
        assert_eq!(cache_hit_percent(&Usage::default()), None);
        let hit = |input: u32, cache: u32| Usage {
            input_tokens: input,
            output_tokens: 0,
            cache_read_tokens: cache,
            reasoning_tokens: 0,
        };
        assert_eq!(cache_hit_percent(&hit(100, 0)), Some(0));
        assert_eq!(cache_hit_percent(&hit(100, 50)), Some(33));
        assert_eq!(cache_hit_percent(&hit(50, 50)), Some(50));
        assert_eq!(cache_hit_percent(&hit(0, 50)), Some(100));
    }

    #[test]
    fn effort_and_model_follow_the_state() {
        let mut bar = bar();
        assert_eq!(bar.model_label(), format!("{MODEL} high"));

        bar.sync(&AppState {
            model: "mini".into(),
            ..idle_state()
        });
        assert_eq!(bar.model_label(), "mini");
    }

    #[test]
    fn usage_events_replace_the_row_usage() {
        let mut bar = bar();
        bar.usage = Usage::default();
        bar.on_event(&agent_event(AgentEvent::Turn(TurnEvent::Completed {
            usage: usage(),
            duration_ms: 0,
        })));
        assert_eq!(bar.usage, usage());

        let rewound = Usage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
        };
        bar.sync(&AppState {
            last_turn_usage: rewound,
            ..idle_state()
        });
        assert_eq!(bar.usage, rewound);
    }

    #[test]
    fn a_running_turn_keeps_reporting_its_own_usage() {
        let mut bar = bar();
        bar.usage = usage();
        bar.sync(&AppState {
            phase: AppPhase::Running {
                turn_id: TurnId::next(),
            },
            last_turn_usage: Usage::default(),
            ..idle_state()
        });
        assert_eq!(bar.usage, usage());
    }

    #[test]
    fn the_context_gauge_follows_the_reported_usage() {
        let mut bar = bar().with_context(0, Some(100_000));
        bar.on_event(&agent_event(AgentEvent::Usage { usage: usage() }));
        assert_eq!(
            bar.context_label().as_deref(),
            Some("ctx 1%"),
            "prompt-side tokens are the input and the cache reads (1989 of 100k)"
        );

        bar.sync(&AppState {
            context_window: Some(1_989),
            context_tokens: 1_989,
            ..idle_state()
        });
        assert_eq!(
            bar.context_label().as_deref(),
            Some("ctx 100%"),
            "the window is the frontend's own, not the usage report's"
        );
    }

    #[test]
    fn a_history_change_drops_the_reply() {
        let mut bar = bar();
        bar.on_event(&AppEvent::notification("Copied!"));
        bar.on_event(&AppEvent::new(AppEventKind::HistoryChanged {
            reason: oven_app::HistoryChangeReason::Rewound,
        }));
        assert!(!bar.has_reply());
    }

    #[test]
    fn syncing_state_keeps_the_reply() {
        let mut bar = bar();
        bar.on_event(&AppEvent::notification("Copied!"));
        bar.sync(&idle_state());
        assert!(bar.has_reply());
    }

    #[test]
    fn reply_toast_sits_on_the_bottom_right() {
        let mut bar = bar();
        bar.on_event(&AppEvent::notification(SHORT_REPLY));
        let buf = toast_buffer(&bar, 40, 6);
        assert!(buffer_text(&buf).contains(SHORT_REPLY));
        assert_eq!(buf.area, Rect::new(0, 0, 40, 6));
        assert!(buffer_text(&buf).contains('╭'));
        assert!(buffer_text(&buf).contains('╰'));
        let orange = Color::Rgb(255, 140, 0);
        assert!(
            buf.content()
                .iter()
                .any(|cell| cell.symbol() == "c" && cell.style().fg == Some(orange))
        );
    }

    #[test]
    fn a_short_toast_fits_its_text() {
        let mut bar = bar();
        bar.on_event(&AppEvent::notification(SHORT_REPLY));
        let buf = toast_buffer(&bar, 40, 6);

        let top = (0..6)
            .find(|y| (0..40).any(|x| buf[(x, *y)].symbol() == "╭"))
            .unwrap();
        let bottom = (0..6)
            .find(|y| (0..40).any(|x| buf[(x, *y)].symbol() == "╰"))
            .unwrap();
        let left = (0u16..40).find(|x| buf[(*x, top)].symbol() == "╭").unwrap();
        let right = (0u16..40).find(|x| buf[(*x, top)].symbol() == "╮").unwrap();

        assert_eq!(
            right - left + 1,
            u16::try_from(SHORT_REPLY.width()).unwrap() + FRAME_SPAN,
            "the box closes in around its text"
        );
        assert_eq!(
            bottom - top + 1,
            FRAME_SPAN + 1,
            "a one-row reply is a one-row box"
        );
        assert!(left > 0, "the box stays clear of the left edge");
    }

    #[test]
    fn toast_never_rises_above_its_anchor() {
        let mut bar = bar();
        bar.on_event(&AppEvent::notification("reply ".repeat(120)));

        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|f| bar.draw_reply_overlay(f, Rect::new(0, 1, 40, 9)))
            .unwrap();
        let buf = terminal.backend().buffer();
        let top: String = (0..40).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(top.trim(), "", "{top:?}");
        assert!(buffer_text(buf).contains("reply"));
    }

    #[test]
    fn repeat_notification_flashes_before_it_is_read() {
        let mut bar = bar();
        bar.on_event(&AppEvent::notification("Copied!"));
        assert!(!bar.is_flashing());

        bar.on_event(&AppEvent::notification("Copied!"));
        assert!(bar.is_flashing());
        assert!(!buffer_text(&toast_buffer(&bar, 40, 6)).contains("Copied!"));

        bar.flash_until = Some(Instant::now() - Duration::from_millis(1));
        assert!(buffer_text(&toast_buffer(&bar, 40, 6)).contains("Copied!"));
    }

    #[test]
    fn reply_expires_once_the_ttl_elapses() {
        let mut bar = bar();
        bar.on_event(&AppEvent::notification("Copied!"));
        assert!(!bar.expire_reply());

        bar.reply_until = Some(Instant::now() - Duration::from_millis(1));
        assert!(bar.expire_reply());
        assert!(!bar.has_reply());
    }

    #[test]
    fn a_finished_flash_asks_for_another_paint() {
        let mut bar = bar();
        bar.on_event(&AppEvent::notification("Copied!"));
        bar.on_event(&AppEvent::notification("Copied!"));
        bar.flash_until = Some(Instant::now() - Duration::from_millis(1));
        assert!(bar.expire_reply());
        assert!(bar.has_reply());
        assert!(!bar.is_flashing());
    }

    #[test]
    fn a_new_notification_replaces_the_previous_one() {
        let mut bar = bar();
        bar.on_event(&AppEvent::notification("Copied!"));
        bar.on_event(&AppEvent::notification("Model changed"));
        assert_eq!(bar.reply.as_deref(), Some("Model changed"));
    }

    #[test]
    fn compaction_replaces_the_context_share() {
        let mut bar = bar().with_context(42_000, Some(100_000));
        let state = State::new();
        assert!(row(120, &bar, &state).contains(CTX));

        bar.on_event(&AppEvent::new(AppEventKind::Compaction(
            CompactionEvent::Started,
        )));
        assert!(row(120, &bar, &state).contains(COMPACTING));

        bar.on_event(&AppEvent::new(AppEventKind::Compaction(
            CompactionEvent::Completed {
                before_tokens: 100,
                after_tokens: 10,
            },
        )));
        bar.on_event(&AppEvent::new(AppEventKind::Compaction(
            CompactionEvent::Started,
        )));
        bar.on_event(&AppEvent::new(AppEventKind::Compaction(
            CompactionEvent::Failed {
                error: "boom".into(),
            },
        )));
        let rendered = row(120, &bar, &state);
        assert!(!rendered.contains(COMPACTING), "{rendered}");
        assert!(rendered.contains(CTX), "{rendered}");
    }

    #[test]
    fn context_share_stays_hidden_without_window_or_tokens() {
        assert_eq!(context_percent(0, Some(100)), None);
        assert_eq!(context_percent(50, None), None);
        assert_eq!(context_percent(50, Some(0)), None);
        assert_eq!(context_percent(50, Some(100)), Some(50));

        let bar = bar().with_context(42_000, None);
        assert!(!row(120, &bar, &State::new()).contains(CTX));
    }

    #[test]
    fn mode_styles_each_agent_mode() {
        for (mode, color) in [
            (AgentMode::Agent, Color::DarkGray),
            (AgentMode::Plan, Color::LightMagenta),
            (AgentMode::Ask, Color::Green),
        ] {
            let bar = bar();
            let mut state = State::new();
            state.mode = mode;
            let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
            terminal
                .draw(|f| bar.draw_bar(f, f.area(), &state))
                .unwrap();
            let buf = terminal.backend().buffer();
            let rendered: String = (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect();
            let at = rendered
                .find(mode.label())
                .unwrap_or_else(|| panic!("{mode:?}"));
            assert_eq!(buf[(at as u16, 0)].style().fg, Some(color), "{mode:?}");
        }
    }

    #[test]
    fn short_root_keeps_the_last_two_components() {
        assert_eq!(short_root(Path::new("/home/u/code/oven")), "code/oven");
        assert_eq!(short_root(Path::new("/tmp")), "tmp");
        assert_eq!(short_root(Path::new("/")), "/");
    }
}

use std::io::{self, Stdout};

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use oven_agent::AgentEvent;
use oven_app::{AppCmd, AppEvent, AppHandle};
use oven_llm::Usage;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use tokio::sync::broadcast;
use tui_textarea::TextArea;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    User,
    Thinking,
    Text,
    Tool,
    Error,
    System,
}

pub struct Ui {
    handle: AppHandle,
    events: broadcast::Receiver<AppEvent>,
    line_cache: Vec<Line<'static>>,
    streaming: String,
    stream_kind: LineKind,
    input: TextArea<'static>,
    status: String,
    total_usage: Usage,
    busy: bool,
    scroll: u16,
}

impl Ui {
    pub fn new(handle: AppHandle) -> Self {
        let events = handle.subscribe();
        Self {
            handle,
            events,
            line_cache: format_lines(
                LineKind::System,
                "oven — Enter send · Alt-Enter newline · PgUp/PgDn scroll · Esc cancel · Ctrl-C quit",
            ),
            streaming: String::new(),
            stream_kind: LineKind::Text,
            input: new_textarea(),
            status: "ready".into(),
            total_usage: Usage::default(),
            busy: false,
            scroll: 0,
        }
    }

    pub async fn run(mut self) -> io::Result<()> {
        let mut terminal = setup_terminal()?;
        let result = self.event_loop(&mut terminal).await;
        restore_terminal(&mut terminal)?;
        self.handle.shutdown().await;
        result
    }

    async fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> io::Result<()> {
        let mut term_events = EventStream::new();
        terminal.draw(|f| self.draw(f))?;
        loop {
            tokio::select! {
                Some(ev) = term_events.next() => {
                    match ev? {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            if self.handle_key(key)? {
                                break;
                            }
                        }
                        Event::Resize(_, _) => {}
                        _ => continue,
                    }
                }
                result = self.events.recv() => {
                    match result {
                        Ok(ev) => self.apply_event(ev),
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => {
                            self.status = "app closed".into();
                            self.busy = false;
                        }
                    }
                    self.drain_events();
                }
            }
            terminal.draw(|f| self.draw(f))?;
        }
        Ok(())
    }

    fn drain_events(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(ev) => self.apply_event(ev),
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(broadcast::error::TryRecvError::Closed) => {
                    self.status = "app closed".into();
                    self.busy = false;
                    break;
                }
            }
        }
    }

    fn push_row(&mut self, kind: LineKind, text: &str) {
        self.line_cache.extend(format_lines(kind, text));
        self.scroll = 0;
    }

    fn flush_streaming(&mut self) {
        let body = std::mem::take(&mut self.streaming);
        let body = trim_message(&body);
        if !body.is_empty() {
            self.push_row(self.stream_kind, &body);
        }
    }

    fn push_stream(&mut self, kind: LineKind, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.streaming.is_empty() && self.stream_kind != kind {
            self.flush_streaming();
        }
        self.stream_kind = kind;
        self.streaming.push_str(text);
        self.scroll = 0;
    }

    fn apply_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Agent { event, .. } => match event {
                AgentEvent::ThinkingDelta { text, .. } => {
                    self.push_stream(LineKind::Thinking, &text);
                    self.status = "thinking…".into();
                }
                AgentEvent::TextDelta { text, .. } => {
                    self.push_stream(LineKind::Text, &text);
                    self.status = "streaming…".into();
                }
                AgentEvent::ToolStart { name, .. } => {
                    self.flush_streaming();
                    self.status = format!("tool: {name}…");
                    self.push_row(LineKind::Tool, &name);
                }
                AgentEvent::ToolEnd { ok, .. } => {
                    self.status = if ok {
                        "tool done".into()
                    } else {
                        "tool failed".into()
                    };
                }
                AgentEvent::Done { text, usage, .. } => {
                    self.total_usage = usage;
                    if self.stream_kind == LineKind::Text {
                        self.streaming.clear();
                    } else {
                        self.flush_streaming();
                    }
                    let body = trim_message(&text);
                    if !body.is_empty() {
                        self.push_row(LineKind::Text, &body);
                    }
                }
                AgentEvent::Cancelled { .. } => {
                    if !self.streaming.is_empty() {
                        let kind = self.stream_kind;
                        let partial = trim_message(&std::mem::take(&mut self.streaming));
                        if !partial.is_empty() {
                            self.push_row(kind, &format!("{partial}…"));
                        }
                    }
                    self.push_row(LineKind::System, "cancelled");
                    self.status = "cancelled".into();
                }
            },
            AppEvent::Idle { .. } => {
                self.flush_streaming();
                self.busy = false;
                if self.status == "streaming…"
                    || self.status == "thinking…"
                    || self.status.starts_with("tool:")
                {
                    self.status = "ready".into();
                }
                if self.status == "cancelled" {
                    self.status = "ready".into();
                }
            }
            AppEvent::Error { message, .. } => {
                self.flush_streaming();
                self.push_row(LineKind::Error, &message);
                self.status = "error".into();
            }
        }
    }

    /// Returns true when the UI should exit.
    fn handle_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.busy {
                    let _ = self.handle.send(AppCmd::Cancel);
                }
                return Ok(true);
            }
            KeyCode::Esc => {
                if self.busy {
                    let _ = self.handle.send(AppCmd::Cancel);
                    self.status = "cancelling…".into();
                }
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_add(1);
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(1);
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) && !self.busy => {
                self.input.insert_newline();
            }
            KeyCode::Enter if !self.busy => {
                let text = self.input.lines().join("\n");
                let text = text.trim().to_string();
                if !text.is_empty() {
                    self.flush_streaming();
                    self.push_row(LineKind::User, &text);
                    self.input = new_textarea();
                    self.busy = true;
                    self.status = "thinking…".into();
                    if self.handle.send(AppCmd::UserInput(text)).is_err() {
                        self.busy = false;
                        self.status = "app channel closed".into();
                    }
                }
            }
            _ if !self.busy => {
                self.input.input(key);
            }
            _ => {}
        }
        Ok(false)
    }

    fn draw(&mut self, f: &mut ratatui::Frame<'_>) {
        let input_h = (self.input.lines().len() as u16).clamp(1, 8).saturating_add(2);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(input_h),
            ])
            .split(f.area());

        self.draw_transcript(f, chunks[0]);
        self.draw_status(f, chunks[1]);
        self.draw_usage(f, chunks[2]);
        self.draw_input(f, chunks[3]);
    }

    fn draw_transcript(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let width = area.width.saturating_sub(2) as usize;
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(self.line_cache.len() + 8);
        for line in &self.line_cache {
            wrap_line_into(&mut lines, line, width);
        }
        if !self.streaming.is_empty() {
            for line in format_lines(self.stream_kind, &self.streaming) {
                wrap_line_into(&mut lines, &line, width);
            }
        }

        let height = area.height.saturating_sub(2) as usize;
        let total = lines.len();
        let max_scroll = total.saturating_sub(height);
        let scroll = (self.scroll as usize).min(max_scroll);
        let start = total.saturating_sub(height + scroll);
        let end = total.saturating_sub(scroll);
        let visible = lines[start..end].to_vec();

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" conversation ");
        let para = Paragraph::new(visible).block(block);
        f.render_widget(para, area);
    }

    fn draw_status(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let style = if self.busy {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let para = Paragraph::new(Span::styled(format!(" {} ", self.status), style));
        f.render_widget(para, area);
    }

    fn draw_usage(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let text = format_usage(&self.total_usage);
        let para = Paragraph::new(Span::styled(text, Style::default().fg(Color::DarkGray)));
        f.render_widget(para, area);
    }

    fn draw_input(&mut self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let title = if self.busy {
            " input (busy) "
        } else {
            " input "
        };
        self.input
            .set_block(Block::default().borders(Borders::ALL).title(title));
        if self.busy {
            self.input
                .set_style(Style::default().fg(Color::DarkGray));
            self.input.set_cursor_style(Style::default());
            self.input.set_cursor_line_style(Style::default());
        } else {
            self.input.set_style(Style::default());
            self.input
                .set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
            self.input.set_cursor_line_style(Style::default());
        }
        f.render_widget(&self.input, area);
    }
}

fn new_textarea() -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_cursor_line_style(Style::default());
    ta.set_placeholder_text("message…");
    ta
}

fn format_usage(u: &Usage) -> String {
    let i = human(u.input_tokens);
    let o = human(u.output_tokens);
    let mut s = format!(" ↑{i} in · ↓{o} out");
    if u.cache_read_tokens > 0 {
        s.push_str(&format!(" · cache {}", human(u.cache_read_tokens)));
    }
    if u.reasoning_tokens > 0 {
        s.push_str(&format!(" · reasoning {}", human(u.reasoning_tokens)));
    }
    s
}

fn human(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn trim_message(text: &str) -> String {
    text.trim_matches(|c: char| c == '\n' || c == '\r')
        .to_string()
}

const LINE_PREFIX_WIDTH: usize = 11;
const LINE_INDENT: &str = "           ";

fn format_lines(kind: LineKind, text: &str) -> Vec<Line<'static>> {
    let (prefix, style) = match kind {
        LineKind::User => (
            "you      | ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        LineKind::Thinking => ("thinking | ", Style::default().fg(Color::DarkGray)),
        LineKind::Text => ("text     | ", Style::default().fg(Color::Green)),
        LineKind::Tool => ("tool     | ", Style::default().fg(Color::Magenta)),
        LineKind::Error => ("err      | ", Style::default().fg(Color::Red)),
        LineKind::System => ("sys      | ", Style::default().fg(Color::DarkGray)),
    };
    let mut lines = Vec::new();
    let mut prev_blank = false;
    for part in text.lines() {
        let blank = part.trim().is_empty();
        if blank && (prev_blank || lines.is_empty()) {
            continue;
        }
        prev_blank = blank;
        let head = if lines.is_empty() { prefix } else { LINE_INDENT };
        lines.push(Line::from(vec![
            Span::styled(head.to_string(), style),
            Span::raw(if blank {
                String::new()
            } else {
                part.to_string()
            }),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), style),
            Span::raw(String::new()),
        ]));
    }
    lines
}

fn wrap_line_into(out: &mut Vec<Line<'static>>, line: &Line<'static>, width: usize) {
    if width == 0 {
        out.push(line.clone());
        return;
    }
    let (prefix, style, body) = match line.spans.as_slice() {
        [head, rest @ ..] => {
            let body: String = rest.iter().map(|s| s.content.as_ref()).collect();
            (head.content.as_ref().to_string(), head.style, body)
        }
        [] => {
            out.push(line.clone());
            return;
        }
    };
    let body_width = width.saturating_sub(LINE_PREFIX_WIDTH).max(1);
    if body.is_empty() {
        out.push(Line::from(vec![
            Span::styled(prefix, style),
            Span::raw(String::new()),
        ]));
        return;
    }
    let mut first = true;
    let mut rest = body.as_str();
    while !rest.is_empty() {
        let (chunk, next) = split_at_width(rest, body_width);
        let head = if first {
            first = false;
            prefix.as_str()
        } else {
            LINE_INDENT
        };
        out.push(Line::from(vec![
            Span::styled(head.to_string(), style),
            Span::raw(chunk.to_string()),
        ]));
        rest = next;
    }
}

fn split_at_width(s: &str, max_width: usize) -> (&str, &str) {
    if max_width == 0 {
        return ("", s);
    }
    if s.width() <= max_width {
        return (s, "");
    }
    let mut width = 0;
    for (i, ch) in s.char_indices() {
        let cw = ch.width().unwrap_or(0);
        if width + cw > max_width {
            if i == 0 {
                let next = ch.len_utf8();
                return (&s[..next], &s[next..]);
            }
            return (&s[..i], &s[i..]);
        }
        width += cw;
    }
    (s, "")
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

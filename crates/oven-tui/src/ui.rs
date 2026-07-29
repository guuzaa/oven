use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use oven_agent::AgentEvent;
use oven_app::{AppCmd, AppEvent, AppHandle};
use oven_llm::Usage;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tokio::sync::broadcast;

struct TranscriptLine {
    kind: LineKind,
    text: String,
}

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
    transcript: Vec<TranscriptLine>,
    streaming: String,
    stream_kind: LineKind,
    input: String,
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
            transcript: vec![TranscriptLine {
                kind: LineKind::System,
                text: "oven — Enter send · Esc cancel · Ctrl-C quit".into(),
            }],
            streaming: String::new(),
            stream_kind: LineKind::Text,
            input: String::new(),
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
        loop {
            self.drain_events();
            terminal.draw(|f| self.draw(f))?;

            if !event::poll(Duration::from_millis(50))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if self.handle_key(key)? {
                break;
            }
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

    fn flush_streaming(&mut self) {
        let body = std::mem::take(&mut self.streaming);
        let body = trim_message(&body);
        if !body.is_empty() {
            self.transcript.push(TranscriptLine {
                kind: self.stream_kind,
                text: body,
            });
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
                    self.transcript.push(TranscriptLine {
                        kind: LineKind::Tool,
                        text: name,
                    });
                }
                AgentEvent::ToolEnd { ok, .. } => {
                    self.status = if ok {
                        "tool done".into()
                    } else {
                        "tool failed".into()
                    };
                }
                AgentEvent::Done { text, usage, .. } => {
                    self.total_usage += usage;
                    if self.streaming.is_empty() {
                        let body = trim_message(&text);
                        if !body.is_empty() {
                            self.transcript.push(TranscriptLine {
                                kind: LineKind::Text,
                                text: body,
                            });
                        }
                    } else {
                        self.flush_streaming();
                    }
                }
                AgentEvent::Cancelled { .. } => {
                    if !self.streaming.is_empty() {
                        let kind = self.stream_kind;
                        let partial = trim_message(&std::mem::take(&mut self.streaming));
                        if !partial.is_empty() {
                            self.transcript.push(TranscriptLine {
                                kind,
                                text: format!("{partial}…"),
                            });
                        }
                    }
                    self.transcript.push(TranscriptLine {
                        kind: LineKind::System,
                        text: "cancelled".into(),
                    });
                    self.status = "cancelled".into();
                }
            },
            AppEvent::Idle { .. } => {
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
                self.transcript.push(TranscriptLine {
                    kind: LineKind::Error,
                    text: message,
                });
                self.streaming.clear();
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
            KeyCode::Enter => {
                if !self.busy {
                    let text = self.input.trim().to_string();
                    if !text.is_empty() {
                        self.transcript.push(TranscriptLine {
                            kind: LineKind::User,
                            text: text.clone(),
                        });
                        self.input.clear();
                        self.streaming.clear();
                        self.busy = true;
                        self.status = "thinking…".into();
                        if self.handle.send(AppCmd::UserInput(text)).is_err() {
                            self.busy = false;
                            self.status = "app channel closed".into();
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_add(1);
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_sub(1);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push(c);
            }
            _ => {}
        }
        Ok(false)
    }

    fn draw(&self, f: &mut ratatui::Frame<'_>) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(3),
            ])
            .split(f.area());

        self.draw_transcript(f, chunks[0]);
        self.draw_status(f, chunks[1]);
        self.draw_usage(f, chunks[2]);
        self.draw_input(f, chunks[3]);
    }

    fn draw_transcript(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();
        for row in &self.transcript {
            lines.extend(format_lines(row.kind, &row.text));
        }
        if !self.streaming.is_empty() {
            lines.extend(format_lines(self.stream_kind, &self.streaming));
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
        let para = Paragraph::new(visible)
            .block(block)
            .wrap(Wrap { trim: false });
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

    fn draw_input(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let title = if self.busy {
            " input (busy) "
        } else {
            " input "
        };
        let block = Block::default().borders(Borders::ALL).title(title);
        let style = if self.busy {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };
        let para = Paragraph::new(self.input.as_str())
            .style(style)
            .block(block);
        f.render_widget(para, area);
        if !self.busy {
            let cursor_x = area.x + 1 + self.input.chars().count() as u16;
            let cursor_y = area.y + 1;
            f.set_cursor_position((cursor_x.min(area.right().saturating_sub(1)), cursor_y));
        }
    }
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
    let indent = "           ";
    let mut lines = Vec::new();
    let mut prev_blank = false;
    for part in text.lines() {
        let blank = part.trim().is_empty();
        if blank && (prev_blank || lines.is_empty()) {
            continue;
        }
        prev_blank = blank;
        let head = if lines.is_empty() { prefix } else { indent };
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

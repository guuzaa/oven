use std::io::{self, Stdout, Write};

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub fn setup() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;
    Ok(terminal)
}

pub fn restore(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Read one line with no echo, for a secret typed at a plain prompt. Raw mode
/// is what hides the characters, so it is restored before returning whether or
/// not reading succeeded.
pub fn read_secret_line() -> io::Result<String> {
    enable_raw_mode()?;
    let read = read_secret_keys();
    let restored = disable_raw_mode();
    let value = read?;
    restored?;
    Ok(value)
}

fn read_secret_keys() -> io::Result<String> {
    let mut line = String::new();
    loop {
        match event::read()? {
            Event::Key(key) => match key.code {
                KeyCode::Enter => return Ok(line),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                }
                KeyCode::Char(ch) => {
                    line.push(ch);
                    echo('*')?;
                }
                KeyCode::Backspace if line.pop().is_some() => echo('\u{8}')?,
                _ => {}
            },
            Event::Paste(text) => line.push_str(text.trim()),
            _ => {}
        }
    }
}

/// Redraw one typed character, or erase the previous one for `\u{8}`.
fn echo(ch: char) -> io::Result<()> {
    let mut stdout = io::stdout();
    match ch {
        '\u{8}' => write!(stdout, "\u{8} \u{8}")?,
        _ => write!(stdout, "{ch}")?,
    }
    stdout.flush()
}

use ansi_to_tui::IntoText;
use oven_app::ShellInput;
use ratatui::style::Style;
use ratatui::text::Span;

use crate::core::theme;

const PROMPT_IDLE: &str = "› ";
const PROMPT_SHELL: &str = "$ ";
const PLACEHOLDER_SHELL: &str = "shell command…";
const PLACEHOLDER_MESSAGE: &str = "message…";

pub fn is_active(text: &str) -> bool {
    ShellInput::parse(text).is_some()
}

pub fn command(text: &str) -> Option<&str> {
    ShellInput::parse(text)?.command()
}

pub fn prompt(active: bool) -> &'static str {
    if active { PROMPT_SHELL } else { PROMPT_IDLE }
}

pub fn prompt_style(active: bool) -> Style {
    if active {
        theme::shell()
    } else {
        theme::user()
    }
}

pub fn text_style(active: bool) -> Style {
    if active {
        theme::shell()
    } else {
        Style::default()
    }
}

pub fn placeholder(active: bool) -> &'static str {
    if active {
        PLACEHOLDER_SHELL
    } else {
        PLACEHOLDER_MESSAGE
    }
}

/// The spans of a shell result, one list per line. The colour codes the
/// command printed become styles, so the output reads the way it did in the
/// terminal instead of showing the escape codes themselves.
pub fn output_lines(text: &str) -> Vec<Vec<Span<'static>>> {
    match text.into_text() {
        Ok(parsed) => parsed.lines.into_iter().map(|line| line.spans).collect(),
        Err(_) => text
            .lines()
            .map(|line| vec![Span::raw(line.to_string())])
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oven_app::LocalShell;
    use ratatui::style::Color;

    fn envelope() -> String {
        LocalShell {
            command: "ls".into(),
            exit_code: Some(0),
            output: "a.rs".into(),
            error: None,
        }
        .to_string()
    }

    #[test]
    fn composer_uses_bang_not_envelope() {
        assert!(is_active("!ls"));
        assert!(is_active("!"));
        assert!(!is_active("ls"));
        assert!(!is_active("wow!"));
        assert!(!is_active(&envelope()));
        assert_eq!(command("!ls -la"), Some("ls -la"));
        assert_eq!(command("!"), None);
    }

    #[test]
    fn colour_codes_become_styles() {
        let lines = output_lines("\x1b[31mred\x1b[0m plain");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0][0].content.as_ref(), "red");
        assert_eq!(lines[0][0].style.fg, Some(Color::Red));
        assert_eq!(lines[0][1].content.as_ref(), " plain");
        assert_eq!(lines[0][1].style.fg, Some(Color::Reset));
    }

    #[test]
    fn plain_output_keeps_its_lines() {
        let lines = output_lines("a.rs\nb.rs");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0][0].content.as_ref(), "a.rs");
        assert_eq!(lines[1][0].content.as_ref(), "b.rs");
    }
}

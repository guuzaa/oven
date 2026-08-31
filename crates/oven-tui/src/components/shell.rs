use oven_app::ShellInput;
use ratatui::style::Style;

use super::theme;

const PROMPT_IDLE: &str = "› ";
const PROMPT_BUSY: &str = "⋅ ";
const PROMPT_SHELL: &str = "$ ";
const PLACEHOLDER_SHELL: &str = "shell command…";
const PLACEHOLDER_MESSAGE: &str = "message…";

pub fn is_active(text: &str) -> bool {
    ShellInput::parse(text).is_some()
}

pub fn command(text: &str) -> Option<&str> {
    ShellInput::parse(text)?.command()
}

pub fn prompt(busy: bool, active: bool) -> &'static str {
    if active && !busy {
        PROMPT_SHELL
    } else if busy {
        PROMPT_BUSY
    } else {
        PROMPT_IDLE
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use oven_app::LocalShell;

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
}

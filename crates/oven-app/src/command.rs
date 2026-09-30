use crate::commands::SlashRegistry;
use crate::platform::shell::ShellInput;

/// What the user submitted, classified once at the frontend boundary so the
/// runtime and the frontend never sniff the same text for the same syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Chat(String),
    /// An empty command is kept so the runtime can say why nothing ran.
    Shell(String),
    Slash {
        name: String,
        args: String,
    },
    Rewind,
}

impl Input {
    pub(crate) fn parse(text: &str, slash: &SlashRegistry) -> Self {
        if let Some(shell) = ShellInput::parse(text) {
            return Self::Shell(shell.command().unwrap_or_default().to_owned());
        }
        match slash.invocation(text) {
            Some((name, args)) => Self::Slash {
                name: name.to_owned(),
                args: args.to_owned(),
            },
            None => Self::Chat(text.to_owned()),
        }
    }

    /// Whether the user wrote it, as opposed to a control gesture: only
    /// prompts are reported as dropped when the app shuts down.
    pub(crate) fn is_prompt(&self) -> bool {
        !matches!(self, Self::Rewind)
    }

    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Chat(_) => "prompt",
            Self::Shell(_) => "shell",
            Self::Slash { .. } => "slash",
            Self::Rewind => "rewind",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Input {
        Input::parse(text, &SlashRegistry::with_builtin())
    }

    #[test]
    fn ordinary_text_is_chat() {
        assert_eq!(
            parse("why is the build slow?"),
            Input::Chat("why is the build slow?".into())
        );
    }

    #[test]
    fn unknown_slash_text_is_chat() {
        assert_eq!(parse("/nope"), Input::Chat("/nope".into()));
    }

    #[test]
    fn bang_text_is_a_shell_command() {
        assert_eq!(parse("! ls -la"), Input::Shell("ls -la".into()));
        assert_eq!(parse("!"), Input::Shell(String::new()));
    }

    #[test]
    fn registered_commands_carry_their_arguments() {
        assert_eq!(
            parse("/setup name=deepseek api_key=sk-secret"),
            Input::Slash {
                name: "setup".into(),
                args: "name=deepseek api_key=sk-secret".into(),
            }
        );
        assert_eq!(
            parse("/clear"),
            Input::Slash {
                name: "clear".into(),
                args: String::new(),
            }
        );
    }

    #[test]
    fn a_rewind_is_not_a_prompt() {
        assert!(!Input::Rewind.is_prompt());
        assert!(parse("hello").is_prompt());
        assert!(parse("/clear").is_prompt());
        assert!(parse("!ls").is_prompt());
    }
}

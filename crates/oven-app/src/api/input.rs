use crate::commands::SlashRegistry;
use crate::core::input::Input;
use crate::platform::shell::ShellInput;

pub(crate) fn classify(text: &str, slash: &SlashRegistry) -> Input {
    if let Some(shell) = ShellInput::parse(text) {
        return Input::Shell(shell.command().unwrap_or_default().to_owned());
    }
    match slash.invocation(text) {
        Some((name, args)) => Input::Slash {
            name: name.to_owned(),
            args: args.to_owned(),
        },
        None => Input::Chat(text.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Input {
        classify(text, &SlashRegistry::with_builtin())
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
}

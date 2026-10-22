//! Line-oriented prompts for the CLI subcommands: one printed question, one
//! line in, one answer out. Keeping the wizard on lines rather than keys is
//! what makes it pipeable and testable without a terminal.

use std::io::{self, IsTerminal, Write};

use oven_app::AppError;

use crate::platform::terminal;

/// List entries printed before a prompt summarizes the rest.
const MAX_LISTED: usize = 20;

/// The questions a wizard asks. `None` means there is no terminal to ask on,
/// so the caller has to fall back to a flag or fail.
pub(crate) trait Prompter {
    /// Whether a person is actually there to answer.
    fn interactive(&self) -> bool;
    fn say(&mut self, line: &str);
    /// Asks for a line. `Ok(None)` is an empty answer with no default to
    /// fall back on, which is not the same as not being able to ask.
    fn text(&mut self, prompt: &str, default: Option<&str>) -> Result<Option<String>, PromptError>;
    /// Asks for a secret, which is not echoed.
    fn secret(&mut self, prompt: &str) -> Result<Option<String>, PromptError>;
    fn confirm(&mut self, prompt: &str, default: bool) -> bool;
}

/// Why a prompt did not answer. The reasons are kept apart so the error a
/// person reads names the real cause.
#[derive(Debug)]
pub(crate) enum PromptError {
    /// The user pressed Ctrl-C.
    Cancelled,
    /// There is no terminal to ask on.
    NotInteractive,
    /// The input ended before an answer came.
    Ended,
    /// The terminal could not be read.
    Io(io::Error),
}

impl PromptError {
    /// The reason in words a person reading the error can act on.
    pub(crate) fn message(&self) -> String {
        match self {
            PromptError::Cancelled => "cancelled".into(),
            PromptError::NotInteractive => "stdin is not a terminal".into(),
            PromptError::Ended => "input ended before an answer came".into(),
            PromptError::Io(err) => err.to_string(),
        }
    }
}

impl From<PromptError> for AppError {
    fn from(error: PromptError) -> Self {
        match error {
            PromptError::Cancelled => AppError::Runtime("cancelled".into()),
            other => AppError::Runtime(other.message()),
        }
    }
}

/// The prompts of a session bound to the terminal.
pub(crate) struct Stdin {
    interactive: bool,
}

impl Stdin {
    pub(crate) fn new() -> Self {
        Self {
            interactive: io::stdin().is_terminal() && io::stdout().is_terminal(),
        }
    }
}

impl Prompter for Stdin {
    fn interactive(&self) -> bool {
        self.interactive
    }

    fn say(&mut self, line: &str) {
        println!("{line}");
    }

    fn text(&mut self, prompt: &str, default: Option<&str>) -> Result<Option<String>, PromptError> {
        if !self.interactive {
            return Err(PromptError::NotInteractive);
        }
        match default {
            Some(value) => print!("{prompt} [{value}]: "),
            None => print!("{prompt}: "),
        }
        let _ = io::stdout().flush();
        let mut line = String::new();
        let read = io::stdin().read_line(&mut line).map_err(PromptError::Io)?;
        if read == 0 {
            return Err(PromptError::Ended);
        }
        let value = line.trim();
        if value.is_empty() {
            Ok(default.map(str::to_string))
        } else {
            Ok(Some(value.to_string()))
        }
    }

    fn secret(&mut self, prompt: &str) -> Result<Option<String>, PromptError> {
        if !self.interactive {
            return Err(PromptError::NotInteractive);
        }
        print!("{prompt}: ");
        let _ = io::stdout().flush();
        let value = terminal::read_secret_line().map_err(|err| {
            if err.kind() == io::ErrorKind::Interrupted {
                PromptError::Cancelled
            } else {
                PromptError::Io(err)
            }
        })?;
        println!();
        Ok(Some(value))
    }

    fn confirm(&mut self, prompt: &str, default: bool) -> bool {
        if !self.interactive {
            return default;
        }
        let hint = if default { "Y/n" } else { "y/N" };
        print!("{prompt} [{hint}]: ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            return default;
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => true,
            "n" | "no" => false,
            _ => default,
        }
    }
}

/// An answer that has to exist: a person is asked again until they give one,
/// and a session with no terminal fails naming the flag that would have
/// supplied it.
pub(crate) fn required_text(
    p: &mut dyn Prompter,
    prompt: &str,
    default: Option<&str>,
    flag: &str,
) -> Result<String, AppError> {
    loop {
        match p.text(prompt, default) {
            Ok(Some(value)) if !value.is_empty() => return Ok(value),
            // An empty answer is asked again: the field is required, and the
            // terminal is there to answer on.
            Ok(_) => continue,
            Err(PromptError::Cancelled) => return Err(AppError::Runtime("cancelled".into())),
            Err(error) => {
                return Err(AppError::Runtime(format!(
                    "{flag} is required ({})",
                    error.message()
                )));
            }
        }
    }
}

/// Print a list, then read one line: a number picks an entry, anything else is
/// taken as the value itself, so a model id can always be typed by hand.
pub(crate) fn choose(
    p: &mut dyn Prompter,
    prompt: &str,
    items: &[String],
    default: Option<&str>,
    flag: &str,
) -> Result<String, AppError> {
    for (index, item) in items.iter().take(MAX_LISTED).enumerate() {
        p.say(&format!("  {}) {item}", index + 1));
    }
    if items.len() > MAX_LISTED {
        p.say(&format!("  … {} more", items.len() - MAX_LISTED));
    }
    let answer = required_text(p, prompt, default, flag)?;
    match answer.parse::<usize>() {
        Ok(index) if (1..=items.len()).contains(&index) => Ok(items[index - 1].clone()),
        _ => Ok(answer),
    }
}

#[cfg(test)]
pub(crate) mod scripted {
    use std::collections::VecDeque;

    use super::{PromptError, Prompter};

    /// Answers a test hands the wizard in advance. An empty queue panics: the
    /// flow asked one question more than the test expected.
    pub(crate) struct Scripted {
        interactive: bool,
        pub(crate) texts: VecDeque<Option<String>>,
        pub(crate) secrets: VecDeque<Option<String>>,
        pub(crate) confirms: VecDeque<bool>,
        pub(crate) said: Vec<String>,
    }

    impl Scripted {
        pub(crate) fn new() -> Self {
            Self {
                interactive: true,
                texts: VecDeque::new(),
                secrets: VecDeque::new(),
                confirms: VecDeque::new(),
                said: Vec::new(),
            }
        }

        pub(crate) fn silent() -> Self {
            Self {
                interactive: false,
                ..Self::new()
            }
        }

        pub(crate) fn texts(mut self, answers: impl IntoIterator<Item = &'static str>) -> Self {
            self.texts = answers.into_iter().map(|a| Some(a.to_string())).collect();
            self
        }

        pub(crate) fn secrets(mut self, answers: impl IntoIterator<Item = &'static str>) -> Self {
            self.secrets = answers.into_iter().map(|a| Some(a.to_string())).collect();
            self
        }

        pub(crate) fn confirms(mut self, answers: impl IntoIterator<Item = bool>) -> Self {
            self.confirms = answers.into_iter().collect();
            self
        }
    }

    impl Prompter for Scripted {
        fn interactive(&self) -> bool {
            self.interactive
        }

        fn say(&mut self, line: &str) {
            self.said.push(line.to_string());
        }

        fn text(
            &mut self,
            _prompt: &str,
            _default: Option<&str>,
        ) -> Result<Option<String>, PromptError> {
            if !self.interactive {
                return Err(PromptError::NotInteractive);
            }
            Ok(self
                .texts
                .pop_front()
                .expect("the flow asked for more text than the test scripted"))
        }

        fn secret(&mut self, _prompt: &str) -> Result<Option<String>, PromptError> {
            if !self.interactive {
                return Err(PromptError::NotInteractive);
            }
            Ok(self
                .secrets
                .pop_front()
                .expect("the flow asked for more secrets than the test scripted"))
        }

        fn confirm(&mut self, _prompt: &str, default: bool) -> bool {
            if !self.interactive {
                return default;
            }
            self.confirms
                .pop_front()
                .expect("the flow asked for more confirmations than the test scripted")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prompter that answers the way a person who gave up would.
    struct Cancelled;

    impl Prompter for Cancelled {
        fn interactive(&self) -> bool {
            true
        }

        fn say(&mut self, _line: &str) {}

        fn text(
            &mut self,
            _prompt: &str,
            _default: Option<&str>,
        ) -> Result<Option<String>, PromptError> {
            Err(PromptError::Cancelled)
        }

        fn secret(&mut self, _prompt: &str) -> Result<Option<String>, PromptError> {
            Err(PromptError::Cancelled)
        }

        fn confirm(&mut self, _prompt: &str, default: bool) -> bool {
            default
        }
    }

    #[test]
    fn an_empty_answer_asks_again() {
        let mut p = scripted::Scripted::new().texts(["", "grok-4.6"]);
        let answer = required_text(&mut p, "model id", None, "--model").unwrap();
        assert_eq!(answer, "grok-4.6");
    }

    #[test]
    fn a_cancelled_prompt_says_so() {
        let mut p = Cancelled;
        let error = required_text(&mut p, "model id", None, "--model").unwrap_err();
        assert_eq!(error.to_string(), "cancelled");
    }

    #[test]
    fn a_prompt_without_a_terminal_names_the_flag() {
        let mut p = scripted::Scripted::silent();
        let error = required_text(&mut p, "model id", None, "--model").unwrap_err();
        assert_eq!(
            error.to_string(),
            "--model is required (stdin is not a terminal)"
        );
    }
}

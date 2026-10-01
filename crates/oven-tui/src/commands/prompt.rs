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
    fn text(&mut self, prompt: &str, default: Option<&str>) -> Option<String>;
    fn secret(&mut self, prompt: &str) -> Option<String>;
    fn confirm(&mut self, prompt: &str, default: bool) -> bool;
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

    fn text(&mut self, prompt: &str, default: Option<&str>) -> Option<String> {
        if !self.interactive {
            return None;
        }
        match default {
            Some(value) => print!("{prompt} [{value}]: "),
            None => print!("{prompt}: "),
        }
        let _ = io::stdout().flush();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).ok()? == 0 {
            return None;
        }
        let value = line.trim();
        if value.is_empty() {
            default.map(str::to_string)
        } else {
            Some(value.to_string())
        }
    }

    fn secret(&mut self, prompt: &str) -> Option<String> {
        if !self.interactive {
            return None;
        }
        print!("{prompt}: ");
        let _ = io::stdout().flush();
        let value = terminal::read_secret_line().ok();
        println!();
        value
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
            Some(value) if !value.is_empty() => return Ok(value),
            Some(_) => continue,
            None => {
                return Err(AppError::Runtime(format!(
                    "{flag} is required (stdin is not a terminal)"
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

    use super::Prompter;

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

        fn text(&mut self, _prompt: &str, _default: Option<&str>) -> Option<String> {
            if !self.interactive {
                return None;
            }
            self.texts
                .pop_front()
                .expect("the flow asked for more text than the test scripted")
        }

        fn secret(&mut self, _prompt: &str) -> Option<String> {
            if !self.interactive {
                return None;
            }
            self.secrets
                .pop_front()
                .expect("the flow asked for more secrets than the test scripted")
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

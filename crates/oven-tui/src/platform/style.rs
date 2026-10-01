//! ANSI styling for the plain-print subcommands that run outside a session.

use std::env;
use std::io::{self, IsTerminal};

const NO_COLOR: &str = "NO_COLOR";
const RESET: &str = "\x1b[0m";

/// ANSI styling, dropped when stdout is not a terminal or `NO_COLOR` is set.
#[derive(Clone, Copy, Default)]
pub(crate) struct Palette {
    color: bool,
}

impl Palette {
    pub(crate) fn detect() -> Self {
        Self {
            color: io::stdout().is_terminal() && env::var_os(NO_COLOR).is_none(),
        }
    }

    pub(crate) fn paint(self, ink: Ink, text: &str) -> String {
        match (self.color, ink.code()) {
            (true, Some(code)) => format!("{code}{text}{RESET}"),
            _ => text.to_owned(),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Ink {
    Plain,
    Bold,
    Dim,
    Red,
    Green,
    Yellow,
}

impl Ink {
    const fn code(self) -> Option<&'static str> {
        match self {
            Self::Plain => None,
            Self::Bold => Some("\x1b[1m"),
            Self::Dim => Some("\x1b[2m"),
            Self::Red => Some("\x1b[31m"),
            Self::Green => Some("\x1b[32m"),
            Self::Yellow => Some("\x1b[33m"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styling_vanishes_without_a_terminal() {
        let plain = Palette::default();
        assert_eq!(plain.paint(Ink::Bold, "x"), "x");
        assert_eq!(plain.paint(Ink::Plain, "x"), "x");
        let color = Palette { color: true };
        assert_eq!(color.paint(Ink::Bold, "x"), "\x1b[1mx\x1b[0m");
        assert_eq!(color.paint(Ink::Plain, "x"), "x");
    }
}

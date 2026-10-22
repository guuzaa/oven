//! What the user submitted, classified once at the frontend boundary so the
//! runtime never sniffs the same text for the same syntax.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
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
    /// Whether the user wrote it, as opposed to a control gesture: only
    /// prompts are reported as dropped when the app shuts down.
    pub fn is_prompt(&self) -> bool {
        !matches!(self, Self::Rewind)
    }

    pub fn kind(&self) -> &'static str {
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

    #[test]
    fn a_rewind_is_not_a_prompt() {
        assert!(!Input::Rewind.is_prompt());
        assert!(Input::Chat("hi".into()).is_prompt());
    }

    #[test]
    fn every_variant_reports_its_kind() {
        assert_eq!(Input::Chat("hi".into()).kind(), "prompt");
        assert_eq!(Input::Shell("ls".into()).kind(), "shell");
        assert_eq!(
            Input::Slash {
                name: "clear".into(),
                args: String::new(),
            }
            .kind(),
            "slash"
        );
        assert_eq!(Input::Rewind.kind(), "rewind");
    }

    #[test]
    fn the_input_survives_the_wire() {
        let input = Input::Slash {
            name: "model".into(),
            args: "gpt-4o".into(),
        };
        let json = serde_json::to_string(&input).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"slash","text":{"name":"model","args":"gpt-4o"}}"#
        );
        assert_eq!(serde_json::from_str::<Input>(&json).unwrap(), input);
    }
}

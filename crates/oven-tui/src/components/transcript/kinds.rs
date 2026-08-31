use ratatui::style::Style;

use super::super::theme;

pub(super) const LINE_PREFIX_WIDTH: usize = 2;
pub(super) const LINE_INDENT: &str = "  ";
pub(super) const SEPARATOR_GLYPH: char = '−';
pub(super) const THINKING_LABEL: &str = "Thinking...";
pub(super) const THOUGHT_LABEL: &str = "Thought";

pub(super) fn thinking_display_label(text: &str) -> &'static str {
    if text == THINKING_LABEL {
        THINKING_LABEL
    } else {
        THOUGHT_LABEL
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LineKind {
    User,
    Shell,
    Thinking,
    Text,
    Tool,
    Diff,
    ToolResult(bool),
    ShellResult(bool),
    Error,
    System,
    Separator,
}

impl LineKind {
    pub(super) fn style(self) -> Style {
        match self {
            LineKind::User => theme::user(),
            LineKind::Shell => theme::shell(),
            LineKind::Thinking => theme::thinking(),
            LineKind::Text => theme::assistant(),
            LineKind::Tool => theme::tool(),
            LineKind::Diff => theme::tool(),
            LineKind::ToolResult(true) | LineKind::ShellResult(true) => theme::ok(),
            LineKind::ToolResult(false) | LineKind::ShellResult(false) => theme::fail(),
            LineKind::Error => theme::error(),
            LineKind::System | LineKind::Separator => theme::dim(),
        }
    }

    pub(super) fn gutter(self) -> &'static str {
        match self {
            LineKind::User => "› ",
            LineKind::Shell | LineKind::Tool => "$ ",
            LineKind::Text => "∙ ",
            LineKind::Thinking => "⋅ ",
            LineKind::ToolResult(_)
            | LineKind::ShellResult(_)
            | LineKind::Diff
            | LineKind::Error
            | LineKind::System
            | LineKind::Separator => "  ",
        }
    }
}

pub(super) struct Row {
    pub kind: LineKind,
    pub text: String,
}

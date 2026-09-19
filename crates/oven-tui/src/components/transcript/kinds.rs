use ratatui::style::Style;

use super::super::collapsible::Collapsible;
use super::super::theme;

pub(super) const LINE_PREFIX_WIDTH: usize = 2;
pub(super) const LINE_INDENT: &str = "  ";
pub(super) const MESSAGE_INDENT: &str = " ";
pub(super) const SEPARATOR_GLYPH: char = '−';

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
            LineKind::System => theme::dim(),
            LineKind::Separator => theme::elapsed(),
        }
    }

    /// Assistant prose draws its gutter on the first line only and aligns the rest under the body.
    pub(super) fn gutter_once(self) -> bool {
        self == LineKind::Text
    }

    pub(super) fn gutter(self) -> &'static str {
        match self {
            LineKind::User => "› ",
            LineKind::Shell => "$ ",
            LineKind::Text => "∙ ",
            LineKind::Thinking
            | LineKind::Tool
            | LineKind::ToolResult(_)
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
    pub collapsible: Option<Collapsible>,
    pub header: Option<usize>,
}

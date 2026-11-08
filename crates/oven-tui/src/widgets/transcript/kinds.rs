use ratatui::style::Style;

use super::collapsible::Collapsible;
use crate::core::theme;

pub(super) const LINE_PREFIX_WIDTH: usize = 2;
pub(super) const LINE_INDENT: &str = "  ";
pub(super) const MESSAGE_INDENT: &str = " ";
pub(super) const COLLAPSED_MARKER: &str = "▸ ";
pub(super) const EXPANDED_MARKER: &str = "▾ ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineKind {
    User,
    Shell,
    Thinking,
    /// One collapsed run of thinking and tool calls between visible text.
    Activity,
    Text,
    Tool,
    Diff,
    ToolResult(bool),
    ShellResult(bool),
    Error,
    /// The turn-end line of a turn that failed.
    Failed,
    System,
    Separator,
}

impl LineKind {
    pub(crate) fn style(self) -> Style {
        match self {
            LineKind::User => theme::user(),
            LineKind::Shell => theme::shell(),
            LineKind::Text => theme::assistant(),
            LineKind::Thinking
            | LineKind::Activity
            | LineKind::Tool
            | LineKind::Diff
            | LineKind::ToolResult(true)
            | LineKind::ShellResult(true)
            | LineKind::System
            | LineKind::Separator => theme::dim(),
            LineKind::ToolResult(false) | LineKind::ShellResult(false) => theme::fail(),
            LineKind::Error => theme::error(),
            LineKind::Failed => theme::fail_bold(),
        }
    }

    /// Assistant prose draws its gutter on the first line only and aligns the rest under the body.
    /// A failure wears its mark the same way: the mark opens the row, the rest hangs under it.
    pub(super) fn gutter_once(self) -> bool {
        matches!(self, LineKind::Text | LineKind::Failed)
    }

    /// A submitted prompt: the header of a turn. It stays pinned while that
    /// turn occupies the top of the viewport.
    pub(super) fn is_prompt(self) -> bool {
        matches!(self, LineKind::User | LineKind::Shell)
    }

    pub(crate) fn gutter(self) -> &'static str {
        match self {
            LineKind::User => "› ",
            LineKind::Shell => "$ ",
            LineKind::Text => "∙ ",
            LineKind::Failed => "✗ ",
            LineKind::Thinking
            | LineKind::Activity
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

/// How a turn ended without an answer.
#[derive(Clone, Copy)]
pub(super) enum Outcome {
    Failed,
    Cancelled,
}

impl Outcome {
    /// The word the turn-end line leads with.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }

    /// The row the outcome is drawn on: a failure is loud, a cancellation
    /// reads like any other turn-end line.
    pub(super) fn kind(self) -> LineKind {
        match self {
            Self::Failed => LineKind::Failed,
            Self::Cancelled => LineKind::Separator,
        }
    }
}

pub(super) struct Row {
    pub kind: LineKind,
    pub text: String,
    pub collapsible: Option<Collapsible>,
    /// Marker lines, in wrapped-line order: the row's own header first, then
    /// one per nested item.
    pub headers: Vec<Header>,
    /// Body lines scrolled off the top of a framed prompt.
    pub prompt_scroll: usize,
    /// Wrapped body lines of a framed prompt, before its window.
    pub prompt_body: usize,
}

/// A clickable ▸/▾ marker. `path` walks nested items by section index, so an
/// empty path is the row itself.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Header {
    pub(super) line: usize,
    pub(super) path: Vec<usize>,
}

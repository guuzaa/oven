use std::collections::HashMap;

use unicode_width::UnicodeWidthStr;

use super::wrap::split_at_width;

const MAX_TOOL_ARG: usize = 80;

#[derive(Default)]
pub(super) struct ToolBurst {
    pub pending: HashMap<String, String>,
    pub entries: Vec<(String, usize, usize)>,
    pub row_open: bool,
    pub wrap_at: usize,
}

impl ToolBurst {
    pub(super) fn bump(&mut self, name: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|(n, _, _)| n == name) {
            entry.1 += 1;
        } else {
            self.entries.push((name.to_string(), 1, 0));
        }
    }

    pub(super) fn bump_failed(&mut self, name: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|(n, _, _)| n == name) {
            entry.2 += 1;
        }
    }
}

pub(super) fn format_tool_summary(entries: &[(String, usize, usize)]) -> String {
    let mut parts: Vec<String> = entries
        .iter()
        .map(|(name, total, _)| {
            if *total > 1 {
                format!("{name} ×{total}")
            } else {
                name.clone()
            }
        })
        .collect();
    let failed: usize = entries.iter().map(|(_, _, f)| *f).sum();
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    parts.join(" · ")
}

pub(super) fn compact_tool_arg(s: &str) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.width() <= MAX_TOOL_ARG {
        return joined;
    }
    let (chunk, _) = split_at_width(&joined, MAX_TOOL_ARG.saturating_sub(1));
    format!("{chunk}…")
}

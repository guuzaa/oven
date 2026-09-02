use std::collections::HashMap;

use unicode_width::UnicodeWidthStr;

use super::wrap::split_at_width;

const MAX_TOOL_ARG: usize = 80;
const MAX_GROUP_DETAILS: usize = 3;

pub(super) struct ToolLabel(String);

impl ToolLabel {
    pub(super) fn from_summary(summary: &str) -> Self {
        let normalized = summary.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.width() <= MAX_TOOL_ARG {
            return Self(normalized);
        }
        let (chunk, _) = split_at_width(&normalized, MAX_TOOL_ARG.saturating_sub(1));
        Self(format!("{chunk}…"))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Default)]
pub(super) struct ToolBurst {
    pending: HashMap<String, ToolLabel>,
    entries: Vec<ToolEntry>,
    pub row_open: bool,
    pub wrap_at: usize,
}

struct ToolEntry {
    action: String,
    details: Vec<String>,
    total: usize,
    failed: usize,
}

impl ToolBurst {
    pub(super) fn start(&mut self, call_id: String, label: ToolLabel) {
        self.bump(label.as_str());
        self.pending.insert(call_id, label);
    }

    pub(super) fn finish(&mut self, call_id: &str, failed: bool) -> bool {
        let Some(label) = self.pending.remove(call_id) else {
            return false;
        };
        if failed {
            self.bump_failed(label.as_str());
        }
        true
    }

    fn bump(&mut self, label: &str) {
        let (action, detail) = split_tool_label(label);
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.action == action) {
            entry.total += 1;
            if !detail.is_empty()
                && entry.details.len() < MAX_GROUP_DETAILS
                && !entry.details.iter().any(|existing| existing == detail)
            {
                entry.details.push(detail.to_string());
            }
        } else {
            self.entries.push(ToolEntry {
                action: action.to_string(),
                details: if detail.is_empty() {
                    Vec::new()
                } else {
                    vec![detail.to_string()]
                },
                total: 1,
                failed: 0,
            });
        }
    }

    fn bump_failed(&mut self, label: &str) {
        let (action, _) = split_tool_label(label);
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.action == action) {
            entry.failed += 1;
        }
    }

    pub(super) fn summary(&self) -> String {
        format_tool_summary(&self.entries)
    }
}

fn split_tool_label(label: &str) -> (&str, &str) {
    label.split_once(' ').unwrap_or((label, ""))
}

fn format_tool_summary(entries: &[ToolEntry]) -> String {
    let mut parts: Vec<String> = entries
        .iter()
        .map(|entry| {
            let details = entry.details.join(" · ");
            match (entry.total, details.is_empty()) {
                (1, true) => entry.action.clone(),
                (1, false) => format!("{} {details}", entry.action),
                (_, true) => format!("{} ×{}", entry.action, entry.total),
                (_, false) => format!("{} ×{} ({details})", entry.action, entry.total),
            }
        })
        .collect();
    let failed: usize = entries.iter().map(|entry| entry.failed).sum();
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::{ToolBurst, ToolLabel};

    #[test]
    fn groups_tool_burst_by_action_and_keeps_compact_details() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), ToolLabel::from_summary("Search todo in src"));
        burst.start(
            "2".into(),
            ToolLabel::from_summary("Search config\n in src"),
        );
        burst.start("3".into(), ToolLabel::from_summary("Read src/main.rs"));
        assert!(burst.finish("2", true));

        assert_eq!(
            burst.summary(),
            "Search ×2 (todo in src · config in src) · Read src/main.rs · 1 failed"
        );
    }
}

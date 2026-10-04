use std::collections::HashMap;

use super::collapsible::{Collapsible, Section};
use super::kinds::LineKind;

pub(super) const TITLE_SEPARATOR: &str = ", ";
const FAILED_LABEL: &str = "failed";
const SINGLE_CALL: usize = 1;

#[derive(PartialEq, Eq)]
enum ToolKind {
    Searched,
    Read,
    Ran,
    Edited,
    Wrote,
    Asked,
    Fetched,
    Other(String),
}

impl From<&str> for ToolKind {
    fn from(value: &str) -> Self {
        match value {
            "Search" | "Find" => Self::Searched,
            "Read" => Self::Read,
            "Ran" => Self::Ran,
            "Edit" => Self::Edited,
            "Write" => Self::Wrote,
            "Ask" => Self::Asked,
            "Fetch" => Self::Fetched,
            _ => Self::Other(value.to_string()),
        }
    }
}

impl ToolKind {
    fn phrase(&self, count: usize) -> String {
        let (verb, noun) = match self {
            Self::Searched => ("Searched", "pattern"),
            Self::Read => ("Read", "file"),
            Self::Ran => ("Ran", "command"),
            Self::Edited => ("Edited", "file"),
            Self::Wrote => ("Wrote", "file"),
            Self::Asked => ("Asked", "question"),
            Self::Fetched => ("Fetched", "page"),
            Self::Other(action) if count == SINGLE_CALL => return action.clone(),
            Self::Other(action) => return format!("{action} ×{count}"),
        };
        let suffix = if count == SINGLE_CALL { "" } else { "s" };
        format!("{verb} {count} {noun}{suffix}")
    }
}

struct ToolGroup {
    kind: ToolKind,
    count: usize,
}

struct Call {
    label: String,
    diff: Option<String>,
    error: Option<String>,
}

#[derive(Default)]
pub(super) struct ToolBurst {
    pending: HashMap<String, usize>,
    groups: Vec<ToolGroup>,
    /// Concrete calls in invocation order, shown when the burst is expanded.
    calls: Vec<Call>,
    failed: usize,
}

impl ToolBurst {
    pub(super) fn start(&mut self, call_id: String, summary: &str, detail: Option<&str>) -> usize {
        let label = normalize(summary);
        let kind = action_of(&label).into();
        if let Some(group) = self.groups.iter_mut().find(|group| group.kind == kind) {
            group.count += 1;
        } else {
            self.groups.push(ToolGroup {
                kind,
                count: SINGLE_CALL,
            });
        }
        let idx = self.calls.len();
        self.pending.insert(call_id, idx);
        self.calls.push(Call {
            label,
            diff: detail.map(str::to_string),
            error: None,
        });
        idx
    }

    pub(super) fn section_at(&self, idx: usize) -> Option<Section> {
        self.calls.get(idx).map(section_of)
    }

    /// Marks a call done: counts the failure, replaces its body with the one
    /// the tool ended on — a question tool shows the answer the user picked —
    /// and keeps the error, beside a diff or under a plain call, so expanding
    /// the burst explains it.
    pub(super) fn finish(
        &mut self,
        call_id: &str,
        detail: Option<&str>,
        failed: bool,
        error: Option<&str>,
    ) -> bool {
        let Some(&idx) = self.pending.get(call_id) else {
            return false;
        };
        self.pending.remove(call_id);
        if failed {
            self.failed += 1;
        }
        let Some(call) = self.calls.get_mut(idx) else {
            return true;
        };
        if let Some(detail) = detail.filter(|text| !text.is_empty()) {
            call.diff = Some(detail.to_string());
        }
        if failed {
            call.error = error.filter(|text| !text.is_empty()).map(str::to_string);
        }
        true
    }

    pub(super) fn title(&self) -> String {
        let mut parts: Vec<String> = self
            .groups
            .iter()
            .map(|group| group.kind.phrase(group.count))
            .collect();
        if self.failed > 0 {
            parts.push(format!("{} {FAILED_LABEL}", self.failed));
        }
        parts.join(TITLE_SEPARATOR)
    }

    /// Whether any call of the burst has yet to report a result.
    pub(super) fn is_running(&self) -> bool {
        !self.pending.is_empty()
    }

    #[cfg(test)]
    pub(super) fn body(&self) -> String {
        self.calls
            .iter()
            .filter(|call| call.diff.is_none())
            .map(|call| call.label.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Calls in invocation order: plain calls as body lines, diff calls as
    /// nested items that each expand to their own diff.
    #[cfg(test)]
    pub(super) fn sections(&self) -> Vec<Section> {
        self.calls.iter().map(section_of).collect()
    }
}

fn section_of(call: &Call) -> Section {
    match &call.diff {
        None => Section::Text(match &call.error {
            Some(error) => format!("{}\n{error}", call.label),
            None => call.label.clone(),
        }),
        Some(diff) => {
            let mut detail = Collapsible::new(diff.clone());
            if let Some(error) = &call.error {
                detail.append(&format!("\n{error}"));
            }
            Section::Item {
                kind: LineKind::Diff,
                title: call.label.clone(),
                detail,
            }
        }
    }
}

fn normalize(summary: &str) -> String {
    summary.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn action_of(label: &str) -> &str {
    label.split_once(' ').map_or(label, |(action, _)| action)
}

#[cfg(test)]
mod tests {
    use super::{Section, ToolBurst};

    fn titles(sections: &[Section]) -> Vec<String> {
        sections
            .iter()
            .map(|section| match section {
                Section::Text(text) => format!("text:{text}"),
                Section::Item { title, .. } => format!("item:{title}"),
            })
            .collect()
    }

    #[test]
    fn groups_calls_by_kind_with_call_details_as_body() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Search todo in src", None);
        burst.start("2".into(), "Search\n config in src", None);
        burst.start("3".into(), "Find **/*.rs in src", None);
        burst.start("4".into(), "Read src/main.rs", None);
        assert!(burst.finish("2", None, true, None));

        assert_eq!(burst.title(), "Searched 3 patterns, Read 1 file, 1 failed");
        assert_eq!(
            burst.body(),
            "Search todo in src\nSearch config in src\nFind **/*.rs in src\nRead src/main.rs"
        );
    }

    #[test]
    fn unknown_tool_carries_its_count_only_when_repeated() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "web_search", None);
        assert_eq!(burst.title(), "web_search");
        burst.start("2".into(), "web_search", None);
        assert_eq!(burst.title(), "web_search ×2");
    }

    #[test]
    fn finish_ignores_calls_outside_the_burst() {
        let mut burst = ToolBurst::default();
        assert!(!burst.finish("missing", None, true, None));
        assert_eq!(burst.title(), "");
    }

    #[test]
    fn file_edits_read_as_edited_files() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Edit src/main.rs", Some("- old\n+ new"));
        burst.start("2".into(), "Write out.txt", Some("+ hi"));
        assert!(burst.finish("1", None, false, None));
        assert_eq!(burst.title(), "Edited 1 file, Wrote 1 file");
        assert_eq!(
            titles(&burst.sections()),
            ["item:Edit src/main.rs", "item:Write out.txt"]
        );
        assert_eq!(burst.body(), "");
    }

    #[test]
    fn a_landed_detail_replaces_what_the_call_ran_with() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Ask which database?", None);
        assert!(burst.finish("1", Some("postgres"), false, None));

        let [Section::Item { title, detail, .. }] = &burst.sections()[..] else {
            panic!("expected one item with a detail");
        };
        assert_eq!(title, "Ask which database?");
        assert_eq!(detail.body(), "postgres");
    }

    #[test]
    fn fetches_are_counted_in_the_burst_title() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Fetch https://example.com", None);
        assert_eq!(burst.title(), "Fetched 1 page");
        burst.start("2".into(), "Fetch https://example.com/docs", None);
        assert_eq!(burst.title(), "Fetched 2 pages");
    }

    #[test]
    fn questions_are_counted_in_the_burst_title() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Ask which database?", None);
        assert_eq!(burst.title(), "Asked 1 question");
        burst.start("2".into(), "Ask sqlite or postgres?", None);
        assert_eq!(burst.title(), "Asked 2 questions");
    }

    #[test]
    fn a_failed_plain_call_keeps_its_error_in_the_body() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "write", None);
        assert!(burst.finish("1", None, true, Some("unknown tool: write")));

        assert_eq!(burst.title(), "write, 1 failed");
        assert_eq!(
            titles(&burst.sections()),
            ["text:write\nunknown tool: write"]
        );
    }

    #[test]
    fn failed_diff_keeps_its_error_inside_the_item() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Edit src/main.rs", Some("- old\n+ new"));
        assert!(burst.finish("1", None, true, Some("old_string not found")));

        assert_eq!(burst.title(), "Edited 1 file, 1 failed");
        let [Section::Item { detail, .. }] = &burst.sections()[..] else {
            panic!("expected one diff item");
        };
        assert_eq!(detail.body(), "- old\n+ new\nold_string not found");
    }

    #[test]
    fn plain_calls_stay_body_lines_between_diff_items() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Ran cargo test", None);
        burst.start("2".into(), "Edit src/main.rs", Some("- old"));
        assert_eq!(
            titles(&burst.sections()),
            ["text:Ran cargo test", "item:Edit src/main.rs"]
        );
    }
}

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
            "Searched" | "Found" => Self::Searched,
            "Read" => Self::Read,
            "Ran" => Self::Ran,
            "Edited" => Self::Edited,
            "Wrote" => Self::Wrote,
            "Asked" => Self::Asked,
            "Fetched" => Self::Fetched,
            _ => Self::Other(value.to_string()),
        }
    }
}

impl ToolKind {
    /// The word a call of this kind leads with.
    fn verb(&self) -> &str {
        match self {
            Self::Searched => "Searched",
            Self::Read => "Read",
            Self::Ran => "Ran",
            Self::Edited => "Edited",
            Self::Wrote => "Wrote",
            Self::Asked => "Asked",
            Self::Fetched => "Fetched",
            Self::Other(action) => action,
        }
    }

    /// Whether the calls of this kind name the file they touched.
    fn names_files(&self) -> bool {
        matches!(self, Self::Edited | Self::Wrote)
    }

    fn phrase(&self, count: usize) -> String {
        let noun = match self {
            Self::Searched => "pattern",
            Self::Read => "file",
            Self::Ran => "command",
            Self::Edited | Self::Wrote => "file",
            Self::Asked => "question",
            Self::Fetched => "page",
            Self::Other(_) => {
                return if count == SINGLE_CALL {
                    self.verb().to_owned()
                } else {
                    format!("{} ×{count}", self.verb())
                };
            }
        };
        let suffix = if count == SINGLE_CALL { "" } else { "s" };
        format!("{} {count} {noun}{suffix}", self.verb())
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
    failed: bool,
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
            failed: false,
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
        call.failed = failed;
        if let Some(detail) = detail.filter(|text| !text.is_empty()) {
            call.diff = Some(detail.to_string());
        }
        if failed {
            call.error = error.filter(|text| !text.is_empty()).map(str::to_string);
            let kind = action_of(&self.calls[idx].label).into();
            self.drop_action(kind);
        }
        true
    }

    /// Takes one call out of the title's action counts: a call that failed is
    /// a failure, not a tool action, so the title counts the failure and
    /// leaves out the name of the tool that did not do what it was asked.
    fn drop_action(&mut self, kind: ToolKind) {
        let Some(pos) = self.groups.iter().position(|group| group.kind == kind) else {
            return;
        };
        self.groups[pos].count -= 1;
        if self.groups[pos].count == 0 {
            self.groups.remove(pos);
        }
    }

    pub(super) fn title(&self) -> String {
        let mut parts: Vec<String> = self
            .groups
            .iter()
            .map(|group| self.group_title(group))
            .collect();
        if self.failed > 0 {
            parts.push(format!("{} {FAILED_LABEL}", self.failed));
        }
        parts.join(TITLE_SEPARATOR)
    }

    /// The title of one group of calls: the files it changed when it changed
    /// files, and how much of everything else it did.
    fn group_title(&self, group: &ToolGroup) -> String {
        if !group.kind.names_files() {
            return group.kind.phrase(group.count);
        }
        let files = self.changed_files(&group.kind);
        if files.is_empty() {
            return group.kind.phrase(group.count);
        }
        format!("{} {}", group.kind.verb(), files.join(", "))
    }

    /// The files a kind of call changed, in the order it changed them and
    /// without a repeat: a file edited twice is one file.
    fn changed_files(&self, kind: &ToolKind) -> Vec<&str> {
        let mut files: Vec<&str> = Vec::new();
        for call in &self.calls {
            if call.failed || &ToolKind::from(action_of(&call.label)) != kind {
                continue;
            }
            if let Some(file) = file_of(&call.label)
                && !files.contains(&file)
            {
                files.push(file);
            }
        }
        files
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
            // The diff waits folded: the title says what changed, and the
            // reader opens the diff when they want to read it.
            let mut detail = Collapsible::collapsed(diff.clone());
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

/// The file a call names, if it names one: the rest of its label.
fn file_of(label: &str) -> Option<&str> {
    let (_, rest) = label.split_once(' ')?;
    let rest = rest.trim();
    (!rest.is_empty()).then_some(rest)
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
        burst.start("1".into(), "Searched todo in src", None);
        burst.start("2".into(), "Searched\n config in src", None);
        burst.start("3".into(), "Found **/*.rs in src", None);
        burst.start("4".into(), "Read src/main.rs", None);
        assert!(burst.finish("2", None, true, None));

        assert_eq!(burst.title(), "Searched 2 patterns, Read 1 file, 1 failed");
        assert_eq!(
            burst.body(),
            "Searched todo in src\nSearched config in src\nFound **/*.rs in src\nRead src/main.rs"
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
        burst.start("1".into(), "Edited src/main.rs", Some("- old\n+ new"));
        burst.start("2".into(), "Wrote out.txt", Some("+ hi"));
        assert!(burst.finish("1", None, false, None));
        assert_eq!(burst.title(), "Edited src/main.rs, Wrote out.txt");
        assert_eq!(
            titles(&burst.sections()),
            ["item:Edited src/main.rs", "item:Wrote out.txt"]
        );
        assert_eq!(burst.body(), "");
    }

    #[test]
    fn a_diff_waits_folded() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Edited src/main.rs", Some("- old\n+ new"));

        let [Section::Item { detail, .. }] = &burst.sections()[..] else {
            panic!("expected one item with a detail");
        };
        assert!(!detail.is_expanded(), "the reader opens the diff");
    }

    #[test]
    fn a_file_edited_twice_is_listed_once() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Edited src/main.rs", Some("- old\n+ new"));
        burst.start("2".into(), "Edited src/main.rs", Some("- newer\n+ newer"));
        burst.start("3".into(), "Edited src/lib.rs", Some("- x\n+ y"));

        assert_eq!(burst.title(), "Edited src/main.rs, src/lib.rs");
    }

    #[test]
    fn a_failed_edit_leaves_no_file_in_the_title() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Edited src/main.rs", Some("- old\n+ new"));
        assert!(burst.finish("1", None, true, Some("old_string not found")));

        assert_eq!(burst.title(), "1 failed");
    }

    #[test]
    fn a_landed_detail_replaces_what_the_call_ran_with() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Asked which database?", None);
        assert!(burst.finish("1", Some("postgres"), false, None));

        let [Section::Item { title, detail, .. }] = &burst.sections()[..] else {
            panic!("expected one item with a detail");
        };
        assert_eq!(title, "Asked which database?");
        assert_eq!(detail.body(), "postgres");
    }

    #[test]
    fn fetches_are_counted_in_the_burst_title() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Fetched https://example.com", None);
        assert_eq!(burst.title(), "Fetched 1 page");
        burst.start("2".into(), "Fetched https://example.com/docs", None);
        assert_eq!(burst.title(), "Fetched 2 pages");
    }

    #[test]
    fn questions_are_counted_in_the_burst_title() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Asked which database?", None);
        assert_eq!(burst.title(), "Asked 1 question");
        burst.start("2".into(), "Asked sqlite or postgres?", None);
        assert_eq!(burst.title(), "Asked 2 questions");
    }

    #[test]
    fn a_failed_plain_call_keeps_its_error_in_the_body() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "write", None);
        assert!(burst.finish("1", None, true, Some("unknown tool: write")));

        assert_eq!(burst.title(), "1 failed");
        assert_eq!(
            titles(&burst.sections()),
            ["text:write\nunknown tool: write"]
        );
    }

    #[test]
    fn a_failed_call_names_no_tool_in_the_title() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Ran ls", None);
        assert!(burst.finish("1", None, true, Some("no such file")));

        assert_eq!(burst.title(), "1 failed");
    }

    #[test]
    fn a_failed_call_leaves_the_title_with_the_failure_alone() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Edited src/main.rs", Some("- old\n+ new"));
        burst.start("2".into(), "edit", None);
        assert!(burst.finish("2", None, true, Some("unknown tool: edit")));
        burst.start("3".into(), "edit", None);
        assert!(burst.finish("3", None, true, Some("unknown tool: edit")));

        assert_eq!(burst.title(), "Edited src/main.rs, 2 failed");
    }

    #[test]
    fn a_failed_call_shares_no_count_with_the_call_it_mimics() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Edited src/main.rs", Some("- old\n+ new"));
        burst.start("2".into(), "Edited", None);
        assert!(burst.finish("2", None, true, Some("unknown tool: Edited")));

        assert_eq!(burst.title(), "Edited src/main.rs, 1 failed");
    }

    #[test]
    fn failed_diff_keeps_its_error_inside_the_item() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Edited src/main.rs", Some("- old\n+ new"));
        assert!(burst.finish("1", None, true, Some("old_string not found")));

        assert_eq!(burst.title(), "1 failed");
        let [Section::Item { detail, .. }] = &burst.sections()[..] else {
            panic!("expected one diff item");
        };
        assert_eq!(detail.body(), "- old\n+ new\nold_string not found");
    }

    #[test]
    fn plain_calls_stay_body_lines_between_diff_items() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Ran cargo test", None);
        burst.start("2".into(), "Edited src/main.rs", Some("- old"));
        assert_eq!(
            titles(&burst.sections()),
            ["text:Ran cargo test", "item:Edited src/main.rs"]
        );
    }
}

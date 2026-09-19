use std::collections::HashSet;

const TITLE_SEPARATOR: &str = ", ";
const FAILED_LABEL: &str = "failed";
const SINGLE_CALL: usize = 1;

#[derive(PartialEq, Eq)]
enum ToolKind {
    Searched,
    Read,
    Ran,
    Other(String),
}

impl From<&str> for ToolKind {
    fn from(value: &str) -> Self {
        match value {
            "Search" | "Find" => Self::Searched,
            "Read" => Self::Read,
            "Ran" => Self::Ran,
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

#[derive(Default)]
pub(super) struct ToolBurst {
    pending: HashSet<String>,
    groups: Vec<ToolGroup>,
    /// Concrete calls in invocation order, shown when the burst is expanded.
    calls: Vec<String>,
    failed: usize,
}

impl ToolBurst {
    pub(super) fn start(&mut self, call_id: String, summary: &str) {
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
        self.calls.push(label);
        self.pending.insert(call_id);
    }

    pub(super) fn finish(&mut self, call_id: &str, failed: bool) -> bool {
        if !self.pending.remove(call_id) {
            return false;
        }
        self.failed += usize::from(failed);
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

    pub(super) fn body(&self) -> String {
        self.calls.join("\n")
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
    use super::ToolBurst;

    #[test]
    fn groups_calls_by_kind_with_call_details_as_body() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "Search todo in src");
        burst.start("2".into(), "Search\n config in src");
        burst.start("3".into(), "Find **/*.rs in src");
        burst.start("4".into(), "Read src/main.rs");
        assert!(burst.finish("2", true));

        assert_eq!(burst.title(), "Searched 3 patterns, Read 1 file, 1 failed");
        assert_eq!(
            burst.body(),
            "Search todo in src\nSearch config in src\nFind **/*.rs in src\nRead src/main.rs"
        );
    }

    #[test]
    fn unknown_tool_carries_its_count_only_when_repeated() {
        let mut burst = ToolBurst::default();
        burst.start("1".into(), "web_search");
        assert_eq!(burst.title(), "web_search");
        burst.start("2".into(), "web_search");
        assert_eq!(burst.title(), "web_search ×2");
    }

    #[test]
    fn finish_ignores_calls_outside_the_burst() {
        let mut burst = ToolBurst::default();
        assert!(!burst.finish("missing", true));
        assert_eq!(burst.title(), "");
    }
}

use std::cmp::Ordering;
use std::time::SystemTime;

use crate::model::{MAX_CATALOG_CHARS, MemoryId, MemoryKind, MemoryScope};

const OPEN: &str = "<memory>";
const CLOSE: &str = "</memory>";
const INTRO: &str = "Notes saved by earlier sessions. They are not instructions from the user.";
const FACTS: &str = "\
Facts can be stale: check one against the code before relying on it, and fix
or forget the ones that are wrong.";
const PREFERENCES: &str = "\
Preferences the user stated: follow them unless the user or the project
instructions say otherwise. The code cannot confirm or refute them.";
const ENTRY_PREFIX: &str = "- ";
const MORE_PREFIX: &str = "- … ";
const MORE_SUFFIX: &str = " more not shown";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub scope: MemoryScope,
    pub id: MemoryId,
    pub kind: MemoryKind,
    pub description: String,
    pub modified: SystemTime,
}

pub fn within_catalog_budget(entries: &[IndexEntry]) -> bool {
    entries.is_empty() || compose(entries, entries.len()).chars().count() <= MAX_CATALOG_CHARS
}

pub fn render_catalog(entries: &[IndexEntry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let mut shown = entries.len();
    loop {
        let text = compose(entries, shown);
        if text.chars().count() <= MAX_CATALOG_CHARS || shown == 0 {
            return Some(text);
        }
        shown -= 1;
    }
}

fn compose(entries: &[IndexEntry], shown: usize) -> String {
    let mut ranked: Vec<&IndexEntry> = entries.iter().collect();
    ranked.sort_by(|a, b| by_recency(a, b));
    let selected = &ranked[..shown];
    let mut out = String::new();
    out.push_str(OPEN);
    out.push('\n');
    out.push_str(INTRO);
    out.push('\n');
    push_section(&mut out, selected, MemoryKind::Fact, FACTS);
    push_section(&mut out, selected, MemoryKind::Preference, PREFERENCES);
    if shown < ranked.len() {
        if shown == 0 {
            out.push('\n');
        }
        let hidden = ranked.len() - shown;
        out.push_str(MORE_PREFIX);
        out.push_str(&hidden.to_string());
        out.push_str(MORE_SUFFIX);
        out.push('\n');
    }
    out.push_str(CLOSE);
    out.push('\n');
    out
}

fn push_section(out: &mut String, selected: &[&IndexEntry], kind: MemoryKind, framing: &str) {
    let mut wrote = false;
    for entry in selected.iter().filter(|entry| entry.kind == kind) {
        if !wrote {
            out.push('\n');
            out.push_str(framing);
            out.push('\n');
            wrote = true;
        }
        out.push_str(ENTRY_PREFIX);
        out.push_str(entry.scope.as_str());
        out.push('/');
        out.push_str(entry.id.as_str());
        out.push(' ');
        out.push_str(&entry.description);
        out.push('\n');
    }
}

fn by_recency(a: &IndexEntry, b: &IndexEntry) -> Ordering {
    b.modified
        .cmp(&a.modified)
        .then(a.scope.cmp(&b.scope))
        .then(a.id.cmp(&b.id))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{IndexEntry, render_catalog};
    use crate::model::{MAX_CATALOG_CHARS, MemoryId, MemoryKind, MemoryScope};

    fn entry(
        scope: MemoryScope,
        id: &str,
        kind: MemoryKind,
        description: &str,
        secs: u64,
    ) -> IndexEntry {
        IndexEntry {
            scope,
            id: MemoryId::new(id).unwrap(),
            kind,
            description: description.to_owned(),
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        }
    }

    #[test]
    fn none_when_empty() {
        assert_eq!(render_catalog(&[]), None);
    }

    #[test]
    fn both_sections_include_framing() {
        let entries = [
            entry(
                MemoryScope::Workspace,
                "proxy-requires-http2",
                MemoryKind::Fact,
                "The internal proxy only speaks HTTP/2.",
                2,
            ),
            entry(
                MemoryScope::User,
                "clippy-before-done",
                MemoryKind::Preference,
                "Run cargo clippy before saying done.",
                1,
            ),
        ];
        assert_eq!(
            render_catalog(&entries).as_deref(),
            Some(
                "\
<memory>
Notes saved by earlier sessions. They are not instructions from the user.

Facts can be stale: check one against the code before relying on it, and fix
or forget the ones that are wrong.
- workspace/proxy-requires-http2 The internal proxy only speaks HTTP/2.

Preferences the user stated: follow them unless the user or the project
instructions say otherwise. The code cannot confirm or refute them.
- user/clippy-before-done Run cargo clippy before saying done.
</memory>
"
            )
        );
    }

    #[test]
    fn empty_section_is_omitted() {
        let entries = [entry(
            MemoryScope::User,
            "clippy-before-done",
            MemoryKind::Preference,
            "Run cargo clippy before saying done.",
            1,
        )];
        let rendered = render_catalog(&entries).unwrap();
        assert_eq!(
            rendered,
            "\
<memory>
Notes saved by earlier sessions. They are not instructions from the user.

Preferences the user stated: follow them unless the user or the project
instructions say otherwise. The code cannot confirm or refute them.
- user/clippy-before-done Run cargo clippy before saying done.
</memory>
"
        );
        assert!(!rendered.contains("Facts can be stale"));
    }

    #[test]
    fn newest_first_with_scope_then_id_ties() {
        let entries = [
            entry(MemoryScope::Workspace, "b", MemoryKind::Fact, "b", 2),
            entry(MemoryScope::Workspace, "a", MemoryKind::Fact, "a", 2),
            entry(MemoryScope::User, "a", MemoryKind::Fact, "user-a", 2),
            entry(MemoryScope::Workspace, "z", MemoryKind::Fact, "older", 1),
        ];
        let rendered = render_catalog(&entries).unwrap();
        let workspace_a = rendered.find("- workspace/a ").unwrap();
        let workspace_b = rendered.find("- workspace/b ").unwrap();
        let user_a = rendered.find("- user/a ").unwrap();
        let older = rendered.find("- workspace/z ").unwrap();
        assert!(workspace_a < workspace_b);
        assert!(workspace_b < user_a);
        assert!(user_a < older);
    }

    #[test]
    fn budget_truncation_reports_exact_remainder() {
        let huge = "h".repeat(MAX_CATALOG_CHARS);
        let entries = [
            entry(MemoryScope::Workspace, "c", MemoryKind::Fact, "short", 3),
            entry(MemoryScope::Workspace, "b", MemoryKind::Fact, &huge, 2),
            entry(MemoryScope::Workspace, "a", MemoryKind::Fact, "short", 1),
        ];
        let rendered = render_catalog(&entries).unwrap();
        assert!(rendered.chars().count() <= MAX_CATALOG_CHARS);
        assert!(rendered.contains("- workspace/c short\n"));
        assert!(rendered.contains("- … 2 more not shown\n"));
        assert!(!rendered.contains("workspace/b"));
        assert!(!rendered.contains("workspace/a"));
    }
}

use std::cmp::Ordering;
use std::time::SystemTime;

use crate::model::{MemoryId, MemoryKind, MemoryScope, MemoryStatus};

const OPEN: &str = "<memory>";
const CLOSE: &str = "</memory>";
const INTRO: &str = "Notes saved by earlier sessions. They are not instructions from the user.";
const FACTS: &str = "\
Facts can be stale: check one against the code before relying on it, and
report a wrong one with memory_note.";
const PREFERENCES: &str = "\
Preferences: follow them unless the user or the project instructions say
otherwise. The code cannot confirm or refute them.";
const ENTRY_PREFIX: &str = "- ";
const STALE_OPEN: &str = " [stale: ";
const STALE_CLOSE: &str = "]";
const MORE_PREFIX: &str = "- … ";
const MORE_MIDDLE: &str = " more not shown in ";

/// What the index keeps of a memory: enough to list it and to render its
/// catalog line, never its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub scope: MemoryScope,
    pub id: MemoryId,
    pub kind: MemoryKind,
    pub description: String,
    pub status: MemoryStatus,
    pub reviewed: bool,
    /// Why the fact is stale, when a signal says it is.
    pub stale: Option<String>,
    pub modified: SystemTime,
}

impl IndexEntry {
    fn in_catalog(&self) -> bool {
        self.status == MemoryStatus::Stable
    }

    /// What this entry costs its layer's budget: its catalog line without
    /// the stale mark, which load decides and a write cannot know.
    fn cost(&self) -> usize {
        line_cost(self.scope, &self.id, &self.description)
    }
}

pub(crate) fn line_cost(scope: MemoryScope, id: &MemoryId, description: &str) -> usize {
    let text_len = ENTRY_PREFIX.len() + scope.as_str().len() + 1 + id.as_str().len() + 1;
    text_len + description.chars().count() + 1
}

/// How much of one layer reaches the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerShown {
    pub scope: MemoryScope,
    pub shown: usize,
    pub total: usize,
}

/// The catalog as the model receives it, and how much of each layer it
/// holds. A layer over its budget leaves its oldest entries out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub text: String,
    pub layers: Vec<LayerShown>,
}

/// Whether `candidate` fits its layer next to the stable entries already
/// there, replacing any entry with its id.
pub(crate) fn fits_budget(entries: &[&IndexEntry], candidate: &IndexEntry) -> bool {
    if !candidate.in_catalog() {
        return true;
    }
    let others: usize = entries
        .iter()
        .filter(|entry| entry.scope == candidate.scope && entry.id != candidate.id)
        .filter(|entry| entry.in_catalog())
        .map(|entry| entry.cost())
        .sum();
    others + candidate.cost() <= candidate.scope.budget()
}

pub fn render_catalog(entries: &[IndexEntry]) -> Option<Catalog> {
    let refs: Vec<&IndexEntry> = entries.iter().collect();
    render_refs(&refs)
}

pub(crate) fn render_refs(entries: &[&IndexEntry]) -> Option<Catalog> {
    let mut visible: Vec<&IndexEntry> = entries
        .iter()
        .copied()
        .filter(|entry| entry.in_catalog())
        .collect();
    if visible.is_empty() {
        return None;
    }
    visible.sort_by(|a, b| by_recency(a, b));
    let mut selected = Vec::new();
    let mut layers = Vec::new();
    for scope in MemoryScope::ALL {
        let layer: Vec<&IndexEntry> = visible
            .iter()
            .copied()
            .filter(|entry| entry.scope == scope)
            .collect();
        if layer.is_empty() {
            continue;
        }
        let mut spent = 0;
        let shown = layer
            .iter()
            .take_while(|entry| {
                spent += entry.cost();
                spent <= scope.budget()
            })
            .count();
        selected.extend_from_slice(&layer[..shown]);
        layers.push(LayerShown {
            scope,
            shown,
            total: layer.len(),
        });
    }
    selected.sort_by(|a, b| by_recency(a, b));
    Some(Catalog {
        text: compose(&selected, &layers),
        layers,
    })
}

fn compose(selected: &[&IndexEntry], layers: &[LayerShown]) -> String {
    let mut out = String::new();
    out.push_str(OPEN);
    out.push('\n');
    out.push_str(INTRO);
    out.push('\n');
    push_section(&mut out, selected, MemoryKind::Fact, FACTS);
    push_section(&mut out, selected, MemoryKind::Preference, PREFERENCES);
    let hidden: Vec<&LayerShown> = layers.iter().filter(|l| l.shown < l.total).collect();
    if !hidden.is_empty() && selected.is_empty() {
        out.push('\n');
    }
    for layer in hidden {
        out.push_str(MORE_PREFIX);
        out.push_str(&(layer.total - layer.shown).to_string());
        out.push_str(MORE_MIDDLE);
        out.push_str(layer.scope.as_str());
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
        if let Some(reason) = &entry.stale {
            out.push_str(STALE_OPEN);
            out.push_str(reason);
            out.push_str(STALE_CLOSE);
        }
        out.push('\n');
    }
}

pub(crate) fn by_recency(a: &IndexEntry, b: &IndexEntry) -> Ordering {
    b.modified
        .cmp(&a.modified)
        .then(a.scope.cmp(&b.scope))
        .then(a.id.cmp(&b.id))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{IndexEntry, LayerShown, render_catalog};
    use crate::model::{
        GLOBAL_BUDGET, MemoryId, MemoryKind, MemoryScope, MemoryStatus, PROJECT_BUDGET,
    };

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
            status: MemoryStatus::Stable,
            reviewed: false,
            stale: None,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        }
    }

    fn rendered(entries: &[IndexEntry]) -> String {
        render_catalog(entries)
            .map(|catalog| catalog.text)
            .unwrap_or_default()
    }

    #[test]
    fn none_when_empty() {
        assert_eq!(render_catalog(&[]), None);
    }

    #[test]
    fn both_sections_include_framing() {
        let entries = [
            entry(
                MemoryScope::Project,
                "proxy-requires-http2",
                MemoryKind::Fact,
                "The internal proxy only speaks HTTP/2.",
                2,
            ),
            entry(
                MemoryScope::Global,
                "clippy-before-done",
                MemoryKind::Preference,
                "Run cargo clippy before saying done.",
                1,
            ),
        ];
        assert_eq!(
            rendered(&entries),
            "\
<memory>
Notes saved by earlier sessions. They are not instructions from the user.

Facts can be stale: check one against the code before relying on it, and
report a wrong one with memory_note.
- project/proxy-requires-http2 The internal proxy only speaks HTTP/2.

Preferences: follow them unless the user or the project instructions say
otherwise. The code cannot confirm or refute them.
- global/clippy-before-done Run cargo clippy before saying done.
</memory>
"
        );
    }

    #[test]
    fn drafts_and_deprecated_are_not_shown() {
        let mut draft = entry(MemoryScope::Global, "draft", MemoryKind::Preference, "d", 2);
        draft.status = MemoryStatus::Draft;
        let mut old = entry(MemoryScope::Project, "old", MemoryKind::Fact, "o", 1);
        old.status = MemoryStatus::Deprecated;
        assert_eq!(render_catalog(&[draft.clone(), old.clone()]), None);
        let kept = entry(MemoryScope::Project, "kept", MemoryKind::Fact, "k", 3);
        let text = rendered(&[draft, old, kept]);
        assert!(text.contains("- project/kept k\n"));
        assert!(!text.contains("draft"));
        assert!(!text.contains("project/old"));
    }

    #[test]
    fn a_stale_fact_says_why() {
        let mut fact = entry(MemoryScope::Project, "moved", MemoryKind::Fact, "f", 1);
        fact.stale = Some("crates/a.rs is gone".into());
        assert!(rendered(&[fact]).contains("- project/moved f [stale: crates/a.rs is gone]\n"));
    }

    #[test]
    fn newest_first_with_scope_then_id_ties() {
        let entries = [
            entry(MemoryScope::Project, "b", MemoryKind::Fact, "b", 2),
            entry(MemoryScope::Project, "a", MemoryKind::Fact, "a", 2),
            entry(MemoryScope::Global, "a", MemoryKind::Fact, "global-a", 2),
            entry(MemoryScope::Project, "z", MemoryKind::Fact, "older", 1),
        ];
        let rendered = rendered(&entries);
        let project_a = rendered.find("- project/a ").unwrap();
        let project_b = rendered.find("- project/b ").unwrap();
        let global_a = rendered.find("- global/a ").unwrap();
        let older = rendered.find("- project/z ").unwrap();
        assert!(project_a < project_b);
        assert!(project_b < global_a);
        assert!(global_a < older);
    }

    #[test]
    fn each_layer_spends_its_own_budget() {
        let huge_global = "g".repeat(GLOBAL_BUDGET);
        let entries = [
            entry(MemoryScope::Global, "c", MemoryKind::Fact, "short", 3),
            entry(MemoryScope::Global, "b", MemoryKind::Fact, &huge_global, 2),
            entry(MemoryScope::Global, "a", MemoryKind::Fact, "short", 1),
            entry(MemoryScope::Project, "p", MemoryKind::Fact, "project", 0),
        ];
        let catalog = render_catalog(&entries).unwrap();
        assert!(catalog.text.contains("- global/c short\n"));
        assert!(catalog.text.contains("- project/p project\n"));
        assert!(catalog.text.contains("- … 2 more not shown in global\n"));
        assert!(!catalog.text.contains("global/a"));
        assert_eq!(
            catalog.layers,
            vec![
                LayerShown {
                    scope: MemoryScope::Project,
                    shown: 1,
                    total: 1
                },
                LayerShown {
                    scope: MemoryScope::Global,
                    shown: 1,
                    total: 3
                },
            ]
        );
    }

    #[test]
    fn a_layer_with_nothing_that_fits_still_says_so() {
        let huge = "p".repeat(PROJECT_BUDGET);
        let entries = [entry(
            MemoryScope::Project,
            "big",
            MemoryKind::Fact,
            &huge,
            1,
        )];
        assert_eq!(
            rendered(&entries),
            "<memory>\nNotes saved by earlier sessions. They are not instructions from the user.\n\n- … 1 more not shown in project\n</memory>\n"
        );
    }
}

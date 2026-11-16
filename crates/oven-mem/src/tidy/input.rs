use std::fmt::Write;

use crate::catalog::IndexEntry;
use crate::inbox::Note;
use crate::model::{Memory, MemoryStatus};

const REVIEWED: &str = "reviewed";
const UNREVIEWED: &str = "unreviewed";

/// A curated memory as tidy sees it: whole, with where it came from and
/// whether a signal says it is stale.
#[derive(Debug, Clone)]
pub struct TidyMemory {
    pub memory: Memory,
    /// The workspace root the memory was learned in, when it is known.
    pub origin: Option<String>,
    pub stale: Option<String>,
}

/// What another project remembers, by its key under `~/.oven/projects`.
#[derive(Debug, Clone)]
pub struct Elsewhere {
    pub key: String,
    pub entries: Vec<IndexEntry>,
}

pub struct TidyInput<'a> {
    pub root: &'a str,
    pub notes: &'a [Note],
    pub memories: &'a [TidyMemory],
    pub elsewhere: &'a [Elsewhere],
}

/// The one user message tidy reads.
pub fn render_input(input: &TidyInput<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "<project root=\"{}\">", input.root);
    out.push_str("\n<notes>\n");
    for (n, note) in input.notes.iter().enumerate() {
        let _ = write!(
            out,
            "{}. {} {} {}",
            n + 1,
            note.at,
            note.kind.as_str(),
            note.evidence.as_str()
        );
        if let Some(about) = &note.about {
            let _ = write!(out, " about {about}");
        }
        let _ = writeln!(out, "\n{}", indent(&note.text));
    }
    out.push_str("</notes>\n\n<memories>\n");
    for item in input.memories {
        push_memory(&mut out, item);
    }
    out.push_str("</memories>\n\n<elsewhere>\n");
    for project in input.elsewhere {
        let stable = project
            .entries
            .iter()
            .filter(|entry| entry.status == MemoryStatus::Stable);
        let _ = writeln!(out, "## {}", project.key);
        for entry in stable {
            let _ = writeln!(out, "- {}: {}", entry.kind.as_str(), entry.description);
        }
    }
    out.push_str("</elsewhere>\n");
    out
}

fn push_memory(out: &mut String, item: &TidyMemory) {
    let memory = &item.memory;
    let trust = match memory.is_reviewed() {
        true => REVIEWED,
        false => UNREVIEWED,
    };
    let _ = write!(
        out,
        "## {} ({}, {}, {trust})",
        memory.reference(),
        memory.kind.as_str(),
        memory.status.as_str()
    );
    if let Some(origin) = &item.origin {
        let _ = write!(out, " origin {origin}");
    }
    if let Some(stale) = &item.stale {
        let _ = write!(out, " [stale: {stale}]");
    }
    let _ = writeln!(out, "\ndescription: {}", memory.description);
    if let Some(why) = &memory.why {
        let _ = writeln!(out, "why: {why}");
    }
    if !memory.anchors.is_empty() {
        let _ = writeln!(out, "anchors: {}", memory.anchors.join(", "));
    }
    if !memory.body.trim().is_empty() {
        let _ = writeln!(out, "body:\n{}", indent(memory.body.trim_end()));
    }
    out.push('\n');
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("   {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::{Elsewhere, TidyInput, TidyMemory, render_input};
    use crate::catalog::IndexEntry;
    use crate::inbox::{Evidence, Note};
    use crate::model::{Memory, MemoryId, MemoryKind, MemoryScope, MemoryStatus, Stamp};

    #[test]
    fn renders_notes_memories_and_other_projects() {
        let notes = [Note {
            at: "2026-11-16T10:00:00Z".into(),
            session: Some("s1".into()),
            kind: MemoryKind::Preference,
            evidence: Evidence::UserSaid,
            text: "no narrating\ncomments".into(),
            about: Some("global/comments".into()),
        }];
        let mut reviewed = Memory::new(
            MemoryId::new("comments").unwrap(),
            MemoryScope::Global,
            MemoryKind::Preference,
            "say why, not what",
        );
        reviewed.verified = vec![Stamp::human("me")];
        let mut fact = Memory::new(
            MemoryId::new("proxy").unwrap(),
            MemoryScope::Project,
            MemoryKind::Fact,
            "proxy is http2",
        );
        fact.body = "use prior knowledge\n".into();
        fact.anchors = vec!["a.rs".into()];
        let memories = [
            TidyMemory {
                memory: reviewed,
                origin: Some("/w/zig".into()),
                stale: None,
            },
            TidyMemory {
                memory: fact,
                origin: None,
                stale: Some("a.rs is gone".into()),
            },
        ];
        let mut draft = IndexEntry {
            scope: MemoryScope::Project,
            id: MemoryId::new("d").unwrap(),
            kind: MemoryKind::Fact,
            description: "a draft".into(),
            status: MemoryStatus::Draft,
            reviewed: false,
            stale: None,
            modified: SystemTime::UNIX_EPOCH,
        };
        let stable = IndexEntry {
            description: "tests first".into(),
            kind: MemoryKind::Preference,
            status: MemoryStatus::Stable,
            ..draft.clone()
        };
        draft.id = MemoryId::new("e").unwrap();
        let elsewhere = [Elsewhere {
            key: "w-web".into(),
            entries: vec![draft, stable],
        }];
        let text = render_input(&TidyInput {
            root: "/w/oven",
            notes: &notes,
            memories: &memories,
            elsewhere: &elsewhere,
        });
        assert_eq!(
            text,
            "\
<project root=\"/w/oven\">

<notes>
1. 2026-11-16T10:00:00Z preference user_said about global/comments
   no narrating
   comments
</notes>

<memories>
## global/comments (preference, stable, reviewed) origin /w/zig
description: say why, not what

## project/proxy (fact, stable, unreviewed) [stale: a.rs is gone]
description: proxy is http2
anchors: a.rs
body:
   use prior knowledge

</memories>

<elsewhere>
## w-web
- preference: tests first
</elsewhere>
"
        );
    }
}

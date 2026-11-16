//! `/memory tidy`: gather what tidy reads, make the one model call, apply
//! the plan and consume the notes it read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oven_agent::{Aside, CancellationToken};
use oven_mem::tidy::{
    Elsewhere, TIDY_PROMPT, TidyContext, TidyInput, TidyMemory, apply_plan, parse_plan,
    render_input,
};
use oven_mem::{
    MemoryError, MemoryId, MemoryScope, MemoryStore, Source, layer_entries, project_key,
};
use serde_json::Value;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::warn;

use crate::dirs;
use crate::memory::plural;

pub(crate) const NOTHING_TO_TIDY: &str = "nothing to tidy: no notes and no memories";
pub(crate) const TIDY_FAILED: &str = "tidy failed";
pub(crate) const NOTHING_CHANGED: &str = "nothing was changed";
pub(crate) const NEXT_START: &str = "the catalog changes on the next start";

const MEMORY_DIR: &str = "memory";
const SESSION_EXT: &str = "jsonl";
const SESSION_META: &str = "session_meta";

/// The notes that, once past, start a tidy without being asked.
pub(crate) const AUTO_TIDY_NOTES: usize = 20;
/// The text of the notes one tidy reads. Notes past it wait for the next run,
/// so the prompt stays bounded however fast the inbox grows.
pub(crate) const MAX_TIDY_NOTE_CHARS: usize = 20_000;

/// `/memory tidy`: the report, or the reason it failed, as one text to show.
pub(crate) async fn tidy(store: &MemoryStore, aside: &Aside, cancel: &CancellationToken) -> String {
    run(store, aside, cancel).await.unwrap_or_else(|text| text)
}

/// Whether the inbox holds more notes than a tidy waits for.
pub(crate) async fn notes_are_due(store: &MemoryStore) -> bool {
    match store.inbox() {
        Some(inbox) => inbox.count().await > AUTO_TIDY_NOTES,
        None => false,
    }
}

/// Runs tidy for the project `store` opened. `Ok` is the report to show
/// (including that there was nothing to do); `Err` is why nothing changed,
/// and leaves the inbox for a later run.
pub(crate) async fn run(
    store: &MemoryStore,
    aside: &Aside,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let Some(inbox) = store.inbox() else {
        return Err(MemoryError::LayerUnavailable {
            scope: MemoryScope::Project,
        }
        .to_string());
    };
    let snapshot = inbox
        .read()
        .await
        .map_err(|err| format!("{TIDY_FAILED}: {err}"))?;
    let batch = snapshot.batch(MAX_TIDY_NOTE_CHARS);
    let memories = curated(store, dirs::sessions_dir().as_deref()).await;
    if batch.notes.is_empty() && memories.is_empty() {
        return Ok(NOTHING_TO_TIDY.to_owned());
    }
    let root = dirs::canonical_root(&store.roots().workspace);
    let projects = projects_dir(store);
    let elsewhere = match &projects {
        Some(projects) => elsewhere(projects, &project_key(&root)).await,
        None => Vec::new(),
    };
    let input = render_input(&TidyInput {
        root: &root,
        notes: &batch.notes,
        memories: &memories,
        elsewhere: &elsewhere,
    });
    let reply = aside
        .complete(TIDY_PROMPT, input, cancel)
        .await
        .map_err(|err| format!("{TIDY_FAILED}: {}; {NOTHING_CHANGED}", err.message))?;
    let plan = parse_plan(&reply).map_err(|err| {
        warn!(reply = %reply, "tidy reply is not a plan");
        format!("{err}; {NOTHING_CHANGED}")
    })?;
    let origins = origins(&memories, projects.as_deref());
    let report = apply_plan(
        store,
        plan,
        &TidyContext {
            model: aside.model().as_str(),
            root: &root,
            origins: &origins,
        },
    )
    .await;
    if let Err(err) = inbox.consume(batch.consumed).await {
        warn!(path = %inbox.path().display(), error = %err, "tidy could not consume the inbox");
    }
    let read = batch.notes.len();
    let left = match batch.left {
        0 => String::new(),
        count => format!(", {count} left for the next run"),
    };
    Ok(format!(
        "{}\n\nread {read} {}{left}; {NEXT_START}",
        report.render(),
        plural(read, "note", "notes")
    ))
}

/// Every project and global memory, whole, with where it came from.
async fn curated(store: &MemoryStore, sessions: Option<&Path>) -> Vec<TidyMemory> {
    let mut memories = Vec::new();
    for entry in store.entries() {
        if entry.scope == MemoryScope::Shared {
            continue;
        }
        let memory = match store.read(entry.scope, &entry.id).await {
            Ok(memory) => memory,
            Err(err) => {
                warn!(memory = %format!("{}/{}", entry.scope, entry.id), error = %err, "tidy skipped an unreadable memory");
                continue;
            }
        };
        let origin = origin(&memory.sources, sessions).await;
        memories.push(TidyMemory {
            memory,
            origin,
            stale: entry.stale,
        });
    }
    memories
}

/// The root a memory was first learned in: the earliest source that names
/// one, a `project:` source directly or a session through the root its file
/// records. Later sources are projects that rewrote it.
async fn origin(sources: &[Source], sessions: Option<&Path>) -> Option<String> {
    for source in sources {
        if let Some(root) = source.project_root() {
            return Some(root.to_owned());
        }
        if let (Some(id), Some(sessions)) = (source.session_id(), sessions)
            && let Some(root) = session_root(sessions, id).await
        {
            return Some(root);
        }
    }
    None
}

/// The root a session file's meta record names. The id comes from a
/// memory file, so it must be a plain file name before it is joined.
async fn session_root(sessions: &Path, id: &str) -> Option<String> {
    let plain = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !plain {
        return None;
    }
    let file = fs::File::open(sessions.join(format!("{id}.{SESSION_EXT}")))
        .await
        .ok()?;
    let first = BufReader::new(file).lines().next_line().await.ok()??;
    let meta: Value = serde_json::from_str(&first).ok()?;
    if meta.get("type").and_then(Value::as_str) != Some(SESSION_META) {
        return None;
    }
    meta.get("root").and_then(Value::as_str).map(str::to_owned)
}

/// `~/.oven/projects`, read off the project layer at
/// `~/.oven/projects/<key>/memory`.
fn projects_dir(store: &MemoryStore) -> Option<PathBuf> {
    let project = store.roots().project.as_deref()?;
    project.parent()?.parent().map(Path::to_path_buf)
}

/// What every other project remembers.
async fn elsewhere(projects: &Path, own_key: &str) -> Vec<Elsewhere> {
    let mut out = Vec::new();
    let Ok(mut dirs) = fs::read_dir(projects).await else {
        return out;
    };
    while let Ok(Some(dir)) = dirs.next_entry().await {
        let key = dir.file_name().to_string_lossy().into_owned();
        if key == own_key {
            continue;
        }
        let entries = layer_entries(&dir.path().join(MEMORY_DIR)).await;
        if !entries.is_empty() {
            out.push(Elsewhere { key, entries });
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// For each global memory with a known origin, the layer `demote` sends it
/// to.
fn origins(memories: &[TidyMemory], projects: Option<&Path>) -> BTreeMap<MemoryId, PathBuf> {
    let Some(projects) = projects else {
        return BTreeMap::new();
    };
    memories
        .iter()
        .filter(|item| item.memory.scope == MemoryScope::Global)
        .filter_map(|item| {
            let root = item.origin.as_deref()?;
            let dir = projects.join(project_key(root)).join(MEMORY_DIR);
            Some((item.memory.id.clone(), dir))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use oven_mem::Source;

    use super::origin;

    #[tokio::test]
    async fn a_legacy_session_source_names_the_root_it_ran_in() {
        let tmp = tempdir::TempDir::new("tidy-origin").unwrap();
        std::fs::write(
            tmp.path().join("01a1-b2.jsonl"),
            "{\"type\":\"session_meta\",\"root\":\"/w/web\",\"created_at\":1}\n{\"type\":\"message\"}\n",
        )
        .unwrap();
        let sessions = Some(tmp.path());
        assert_eq!(
            origin(&[Source::session("01a1-b2")], sessions)
                .await
                .as_deref(),
            Some("/w/web")
        );
        assert_eq!(
            origin(
                &[Source::session("01a1-b2"), Source::project("/w/oven")],
                sessions
            )
            .await
            .as_deref(),
            Some("/w/web"),
            "the session the memory was learned in comes before the project that rewrote it"
        );
        assert_eq!(
            origin(
                &[Source::session("missing"), Source::project("/w/oven")],
                sessions
            )
            .await
            .as_deref(),
            Some("/w/oven")
        );
        assert_eq!(
            origin(&[Source::session("../01a1-b2")], sessions).await,
            None
        );
        assert_eq!(origin(&[], sessions).await, None);
    }
}

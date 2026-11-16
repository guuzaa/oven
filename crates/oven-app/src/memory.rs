//! Human-facing memory operations shared by `/memory` and `oven mem`.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use oven_mem::MemoryStore;
use oven_mem::{IndexEntry, MemoryError, MemoryId, MemoryScope, MemoryStatus, NOT_FOUND, Stamp};

use crate::core::error::AppError;
use crate::core::memory::{describe, parse_id, parse_scope};

pub(crate) const NO_MEMORIES: &str = "no memories";
pub(crate) const NO_DRAFTS: &str = "no drafts";
pub(crate) const MEMORY_DISABLED: &str = "memory is disabled";
pub(crate) const USAGE: &str =
    "usage: /memory [review | tidy | show <ref> | rm <ref> | mv <ref> <scope> | approve <ref>]";
pub(crate) const TIDY_IN_APP: &str = "tidy needs a model; run /memory tidy inside oven";
pub(crate) const AMBIGUOUS_MEMORY: &str = "memory id is in more than one layer; qualify it";
pub(crate) const REMOVED_MEMORY: &str = "removed memory";
pub(crate) const MOVED_MEMORY: &str = "moved memory";
pub(crate) const APPROVED_MEMORY: &str = "approved memory";
pub(crate) const NO_EDITOR: &str = "neither VISUAL nor EDITOR is set";
pub(crate) const BUDGET_FULL: &str = "budget full";
pub(crate) const AWAIT_REVIEW: &str = "await /memory review";
pub(crate) const WAIT_FOR_TIDY: &str = "wait for /memory tidy";

const REVIEW: &str = "review";
const TIDY: &str = "tidy";
const SHOW: &str = "show";
const RM: &str = "rm";
const MV: &str = "mv";
const APPROVE: &str = "approve";
const LIST_BULLET: &str = "- ";
const NOT_LOADED: &str = "not loaded";
const CATALOG_LABEL: &str = "catalog";
const REVIEWED_MARK: &str = "reviewed";
const STALE_OPEN: &str = " [stale: ";
const STALE_CLOSE: &str = "]";
const FOOTER_SEPARATOR: &str = "; ";
const USER_VARS: [&str; 2] = ["USER", "USERNAME"];
const UNKNOWN_HUMAN: &str = "local";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemoryAction {
    List,
    Review,
    Tidy,
    Show(MemoryRef),
    Remove(MemoryRef),
    Move(MemoryRef, MemoryScope),
    Approve(MemoryRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemoryRef {
    Scoped { scope: MemoryScope, id: MemoryId },
    Bare(MemoryId),
}

pub(crate) fn parse_command(args: &str) -> Result<MemoryAction, AppError> {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    match tokens.as_slice() {
        [] => Ok(MemoryAction::List),
        [REVIEW] => Ok(MemoryAction::Review),
        [TIDY] => Ok(MemoryAction::Tidy),
        [SHOW, raw] => Ok(MemoryAction::Show(parse_ref(raw)?)),
        [RM, raw] => Ok(MemoryAction::Remove(parse_ref(raw)?)),
        [MV, raw, scope] => Ok(MemoryAction::Move(
            parse_ref(raw)?,
            parse_scope(scope).map_err(AppError::Runtime)?,
        )),
        [APPROVE, raw] => Ok(MemoryAction::Approve(parse_ref(raw)?)),
        _ => Err(AppError::Runtime(USAGE.to_owned())),
    }
}

pub(crate) fn parse_ref(raw: &str) -> Result<MemoryRef, AppError> {
    match raw.split_once('/') {
        Some((scope, id)) => Ok(MemoryRef::Scoped {
            scope: parse_scope(scope).map_err(AppError::Runtime)?,
            id: parse_id(id).map_err(AppError::Runtime)?,
        }),
        None => Ok(MemoryRef::Bare(parse_id(raw).map_err(AppError::Runtime)?)),
    }
}

pub async fn open(root: &Path) -> Arc<MemoryStore> {
    Arc::new(MemoryStore::load(crate::dirs::memory_roots(root)).await)
}

pub async fn list(store: &MemoryStore) -> String {
    apply(store, MemoryAction::List).await
}

pub async fn review(store: &MemoryStore) -> String {
    apply(store, MemoryAction::Review).await
}

pub async fn show(store: &MemoryStore, raw: &str) -> Result<String, AppError> {
    Ok(apply(store, MemoryAction::Show(parse_ref(raw)?)).await)
}

pub async fn remove(store: &MemoryStore, raw: &str) -> Result<String, AppError> {
    Ok(apply(store, MemoryAction::Remove(parse_ref(raw)?)).await)
}

pub async fn move_to(store: &MemoryStore, raw: &str, scope: &str) -> Result<String, AppError> {
    let target = parse_scope(scope).map_err(AppError::Runtime)?;
    Ok(apply(store, MemoryAction::Move(parse_ref(raw)?, target)).await)
}

pub async fn approve(store: &MemoryStore, raw: &str) -> Result<String, AppError> {
    Ok(apply(store, MemoryAction::Approve(parse_ref(raw)?)).await)
}

pub async fn file_path(store: &MemoryStore, raw: &str) -> Result<PathBuf, AppError> {
    let (scope, id) = resolve(store, &parse_ref(raw)?).map_err(AppError::Runtime)?;
    store
        .path(scope, &id)
        .map_err(|err| AppError::Runtime(err.to_string()))
}

pub fn editor_program(visual: Option<&str>, editor: Option<&str>) -> Result<String, AppError> {
    visual
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| editor.map(str::trim).filter(|value| !value.is_empty()))
        .map(str::to_owned)
        .ok_or_else(|| AppError::Runtime(NO_EDITOR.to_owned()))
}

pub(crate) async fn apply(store: &MemoryStore, action: MemoryAction) -> String {
    let result = match action {
        MemoryAction::List => Ok(render_list(store).await),
        MemoryAction::Review => Ok(render_review(store)),
        MemoryAction::Tidy => Err(TIDY_IN_APP.to_owned()),
        MemoryAction::Show(memory_ref) => show_ref(store, &memory_ref).await,
        MemoryAction::Remove(memory_ref) => remove_ref(store, &memory_ref).await,
        MemoryAction::Move(memory_ref, target) => move_ref(store, &memory_ref, target).await,
        MemoryAction::Approve(memory_ref) => approve_ref(store, &memory_ref).await,
    };
    result.unwrap_or_else(|text| text)
}

/// Every memory, one section per layer, newest first, then what the model
/// sees of them and what waits for a person or for tidy.
async fn render_list(store: &MemoryStore) -> String {
    let entries = sorted_entries(store);
    let mut out = match entries.is_empty() {
        true => NO_MEMORIES.to_owned(),
        false => render_layers(&entries),
    };
    let footer = footer(store, &entries).await;
    if !footer.is_empty() {
        out.push_str("\n\n");
        out.push_str(&footer);
    }
    for skip in store.skipped() {
        let _ = write!(
            out,
            "\n{NOT_LOADED}: {}/{} ({})",
            skip.scope, skip.file, skip.reason
        );
    }
    out
}

fn render_review(store: &MemoryStore) -> String {
    let drafts: Vec<IndexEntry> = sorted_entries(store)
        .into_iter()
        .filter(|entry| entry.status == MemoryStatus::Draft)
        .collect();
    match drafts.is_empty() {
        true => NO_DRAFTS.to_owned(),
        false => render_layers(&drafts),
    }
}

fn sorted_entries(store: &MemoryStore) -> Vec<IndexEntry> {
    let mut entries = store.entries();
    entries.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then(left.id.cmp(&right.id))
    });
    entries
}

fn render_layers(entries: &[IndexEntry]) -> String {
    let mut out = String::new();
    for scope in MemoryScope::ALL {
        let mut group = entries
            .iter()
            .filter(|entry| entry.scope == scope)
            .peekable();
        if group.peek().is_none() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        let _ = writeln!(out, "{scope}:");
        for entry in group {
            let _ = write!(
                out,
                "{LIST_BULLET}{} ({}) {}",
                entry.id,
                marks(entry),
                entry.description
            );
            if let Some(reason) = &entry.stale {
                let _ = write!(out, "{STALE_OPEN}{reason}{STALE_CLOSE}");
            }
            out.push('\n');
        }
    }
    let _ = out.pop();
    out
}

/// `fact`, `preference, draft`, `fact, reviewed`: the kind, then whatever
/// sets this memory apart from a stable, unreviewed one.
fn marks(entry: &IndexEntry) -> String {
    let mut marks = vec![entry.kind.as_str()];
    if entry.status != MemoryStatus::Stable {
        marks.push(entry.status.as_str());
    }
    if entry.reviewed {
        marks.push(REVIEWED_MARK);
    }
    marks.join(", ")
}

/// What the model gets to see, layer by layer, and what is waiting. A
/// layer over its budget shows less than it holds.
async fn footer(store: &MemoryStore, entries: &[IndexEntry]) -> String {
    let mut parts = Vec::new();
    if let Some(catalog) = store.catalog() {
        let layers: Vec<String> = catalog
            .layers
            .iter()
            .map(|layer| {
                let mut text = format!("{} {} of {}", layer.scope, layer.shown, layer.total);
                if layer.shown < layer.total {
                    let _ = write!(text, " ({BUDGET_FULL})");
                }
                text
            })
            .collect();
        parts.push(format!("{CATALOG_LABEL}: {}", layers.join(", ")));
    }
    let drafts = entries
        .iter()
        .filter(|entry| entry.status == MemoryStatus::Draft)
        .count();
    if drafts > 0 {
        parts.push(format!(
            "{drafts} {} {AWAIT_REVIEW}",
            plural(drafts, "draft", "drafts")
        ));
    }
    let notes = match store.inbox() {
        Some(inbox) => inbox.count().await,
        None => 0,
    };
    if notes > 0 {
        parts.push(format!(
            "{notes} {} {WAIT_FOR_TIDY}",
            plural(notes, "note", "notes")
        ));
    }
    parts.join(FOOTER_SEPARATOR)
}

pub(crate) fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    match count {
        1 => one,
        _ => many,
    }
}

async fn show_ref(store: &MemoryStore, memory_ref: &MemoryRef) -> Result<String, String> {
    let (scope, id) = resolve(store, memory_ref)?;
    let memory = store
        .read(scope, &id)
        .await
        .map_err(|err| err.to_string())?;
    Ok(describe(&memory))
}

async fn remove_ref(store: &MemoryStore, memory_ref: &MemoryRef) -> Result<String, String> {
    let (scope, id) = resolve(store, memory_ref)?;
    store
        .forget(scope, &id)
        .await
        .map_err(|err| err.to_string())?;
    Ok(format!("{REMOVED_MEMORY}: {scope}/{id}"))
}

async fn move_ref(
    store: &MemoryStore,
    memory_ref: &MemoryRef,
    target: MemoryScope,
) -> Result<String, String> {
    let (scope, id) = resolve(store, memory_ref)?;
    let moved = store
        .move_to(scope, &id, target, reviewer())
        .await
        .map_err(|err| err.to_string())?;
    Ok(format!(
        "{MOVED_MEMORY}: {scope}/{id} → {}",
        moved.reference()
    ))
}

async fn approve_ref(store: &MemoryStore, memory_ref: &MemoryRef) -> Result<String, String> {
    let (scope, id) = resolve(store, memory_ref)?;
    let approved = store
        .approve(scope, &id, reviewer())
        .await
        .map_err(|err| err.to_string())?;
    Ok(format!("{APPROVED_MEMORY}: {}", approved.reference()))
}

/// The person at the keyboard, as an OKF actor.
fn reviewer() -> Stamp {
    let name = USER_VARS
        .iter()
        .find_map(|var| {
            std::env::var(var)
                .ok()
                .filter(|name| !name.trim().is_empty())
        })
        .unwrap_or_else(|| UNKNOWN_HUMAN.to_owned());
    Stamp::human(name.trim())
}

fn resolve(store: &MemoryStore, memory_ref: &MemoryRef) -> Result<(MemoryScope, MemoryId), String> {
    match memory_ref {
        MemoryRef::Scoped { scope, id } if store.contains(*scope, id) => Ok((*scope, id.clone())),
        MemoryRef::Scoped { scope, id } => Err(MemoryError::NotFound {
            scope: *scope,
            id: id.clone(),
        }
        .to_string()),
        MemoryRef::Bare(id) => match store.scopes_of(id).as_slice() {
            [] => Err(format!("{NOT_FOUND}: {id}")),
            [scope] => Ok((*scope, id.clone())),
            scopes => {
                let refs: Vec<String> =
                    scopes.iter().map(|scope| format!("{scope}/{id}")).collect();
                Err(format!("{AMBIGUOUS_MEMORY}: {}", refs.join(" or ")))
            }
        },
    }
}

#[path = "memory_tidy.rs"]
pub(crate) mod tidy;
pub(crate) use tidy::tidy;

#[cfg(test)]
#[path = "memory_test.rs"]
mod memory_test;

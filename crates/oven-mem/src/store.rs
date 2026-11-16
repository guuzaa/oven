use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use tokio::fs;
use tracing::warn;

use crate::catalog::{Catalog, IndexEntry, fits_budget, render_refs};
use crate::error::MemoryError;
use crate::format::{parse, render};
use crate::inbox::Inbox;
use crate::model::{
    MAX_ANCHOR, MAX_ANCHORS, MAX_BODY, MAX_DESCRIPTION, MAX_WHY, Memory, MemoryId, MemoryKind,
    MemoryScope, MemoryStatus, Stamp,
};

const MARKDOWN_EXT: &str = "md";
const TRASH_DIR: &str = ".trash";
const GONE: &str = "is gone";
const PAST_STALE_AFTER: &str = "past stale_after";

/// The directory of each layer, and the workspace anchors resolve against.
/// `None` is a layer that needs a home directory there is not.
#[derive(Debug, Clone)]
pub struct MemoryRoots {
    pub workspace: PathBuf,
    pub project: Option<PathBuf>,
    pub shared: PathBuf,
    pub global: Option<PathBuf>,
    /// The project's inbox file, beside its layer.
    pub inbox: Option<PathBuf>,
}

impl MemoryRoots {
    pub fn dir(&self, scope: MemoryScope) -> Result<&Path, MemoryError> {
        match scope {
            MemoryScope::Project => self.project.as_deref(),
            MemoryScope::Shared => Some(self.shared.as_path()),
            MemoryScope::Global => self.global.as_deref(),
        }
        .ok_or(MemoryError::LayerUnavailable { scope })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutOutcome {
    Created,
    Replaced { previous_description: String },
}

type Index = BTreeMap<(MemoryScope, MemoryId), IndexEntry>;

/// A file in a layer directory that is not a memory, and why it was left
/// out of the index. Written by hand, so it is worth showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFile {
    pub scope: MemoryScope,
    pub file: String,
    pub reason: String,
}

pub struct MemoryStore {
    roots: MemoryRoots,
    index: RwLock<Index>,
    skipped: Vec<SkippedFile>,
}

impl MemoryStore {
    pub async fn load(roots: MemoryRoots) -> Self {
        let mut index = BTreeMap::new();
        let mut skipped = Vec::new();
        for scope in MemoryScope::ALL {
            if let Ok(dir) = roots.dir(scope) {
                load_root(&mut index, &mut skipped, dir, scope, &roots.workspace).await;
            }
        }
        skipped.sort_by(|a, b| (a.scope, &a.file).cmp(&(b.scope, &b.file)));
        Self {
            roots,
            index: RwLock::new(index),
            skipped,
        }
    }

    /// The files the load left out, so a hand edit that broke a memory is
    /// visible instead of silently gone.
    pub fn skipped(&self) -> &[SkippedFile] {
        &self.skipped
    }

    pub fn roots(&self) -> &MemoryRoots {
        &self.roots
    }

    /// The project's inbox, when there is a home directory to keep it in.
    pub fn inbox(&self) -> Option<Inbox> {
        self.roots.inbox.as_deref().map(Inbox::new)
    }

    pub async fn read(&self, scope: MemoryScope, id: &MemoryId) -> Result<Memory, MemoryError> {
        let path = self.path(scope, id)?;
        match fs::read_to_string(&path).await {
            Ok(raw) => parse(id.clone(), scope, &raw),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Err(MemoryError::NotFound {
                scope,
                id: id.clone(),
            }),
            Err(err) => Err(err.into()),
        }
    }

    /// The model's write: validated, kept within its layer's budget, and
    /// never into `shared`.
    pub async fn put(&self, memory: Memory) -> Result<PutOutcome, MemoryError> {
        self.put_replacing(memory, &[]).await
    }

    /// A write that absorbs other memories: they do not count against the
    /// budget, and they go to `.trash/` once the write has landed.
    pub async fn put_replacing(
        &self,
        mut memory: Memory,
        replaces: &[(MemoryScope, MemoryId)],
    ) -> Result<PutOutcome, MemoryError> {
        if memory.scope == MemoryScope::Shared
            || replaces
                .iter()
                .any(|(scope, _)| *scope == MemoryScope::Shared)
        {
            return Err(MemoryError::SharedReadOnly);
        }
        validate(&mut memory)?;
        let path = self.path(memory.scope, &memory.id)?;
        let outcome = match fs::read_to_string(&path).await {
            Ok(raw) => PutOutcome::Replaced {
                previous_description: parse(memory.id.clone(), memory.scope, &raw)
                    .map(|old| old.description)
                    .unwrap_or_default(),
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => PutOutcome::Created,
            Err(err) => return Err(err.into()),
        };
        let candidate = entry_of(&memory, SystemTime::now(), None);
        let fits = {
            let guard = self.read_index();
            let entries: Vec<&IndexEntry> = guard
                .iter()
                .filter(|(key, _)| !replaces.contains(key))
                .map(|(_, entry)| entry)
                .collect();
            fits_budget(&entries, &candidate)
        };
        if !fits {
            return Err(MemoryError::LayerFull {
                scope: memory.scope,
                budget: memory.scope.budget(),
            });
        }
        self.write(&memory).await?;
        let target = (memory.scope, memory.id);
        for (scope, id) in replaces.iter().filter(|key| **key != target) {
            self.forget(*scope, id).await?;
        }
        Ok(outcome)
    }

    /// Moves a memory, unchanged, into a project's layer: this project's,
    /// or another one's that `dir` names.
    pub async fn relocate(
        &self,
        scope: MemoryScope,
        id: &MemoryId,
        dir: &Path,
    ) -> Result<(), MemoryError> {
        let source = self.path(scope, id)?;
        let target = memory_path(dir, id);
        if fs::try_exists(&target).await? {
            return Err(MemoryError::AlreadyExists {
                scope: MemoryScope::Project,
                id: id.clone(),
            });
        }
        let mut memory = self.read(scope, id).await?;
        if self.roots.project.as_deref() == Some(dir) {
            memory.scope = MemoryScope::Project;
            self.write(&memory).await?;
        } else {
            let raw = fs::read_to_string(&source).await?;
            oven_host::write_atomic(&target, raw).await?;
        }
        fs::remove_file(&source).await?;
        self.write_index().remove(&(scope, id.clone()));
        Ok(())
    }

    /// Removes a memory: `global` and `project` into `.trash/`, `shared`
    /// for good, since its history is the repository's.
    pub async fn forget(&self, scope: MemoryScope, id: &MemoryId) -> Result<(), MemoryError> {
        let path = self.path(scope, id)?;
        let result = match scope {
            MemoryScope::Shared => fs::remove_file(&path).await,
            MemoryScope::Project | MemoryScope::Global => trash(&path, id).await,
        };
        match result {
            Ok(()) => {
                self.write_index().remove(&(scope, id.clone()));
                Ok(())
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Err(MemoryError::NotFound {
                scope,
                id: id.clone(),
            }),
            Err(err) => Err(err.into()),
        }
    }

    /// A person moves a memory to another layer, which also reviews it. No
    /// budget applies: the listing says when a layer shows less than it has.
    pub async fn move_to(
        &self,
        scope: MemoryScope,
        id: &MemoryId,
        target: MemoryScope,
        reviewer: Stamp,
    ) -> Result<Memory, MemoryError> {
        let mut memory = self.read(scope, id).await?;
        if target == scope {
            return Ok(memory);
        }
        let target_path = self.path(target, id)?;
        if fs::try_exists(&target_path).await? {
            return Err(MemoryError::AlreadyExists {
                scope: target,
                id: id.clone(),
            });
        }
        memory.scope = target;
        memory.verified.push(reviewer);
        self.write(&memory).await?;
        fs::remove_file(self.path(scope, id)?).await?;
        self.write_index().remove(&(scope, id.clone()));
        Ok(memory)
    }

    /// A person accepts a memory: a draft becomes stable, and any memory
    /// becomes human-reviewed.
    pub async fn approve(
        &self,
        scope: MemoryScope,
        id: &MemoryId,
        reviewer: Stamp,
    ) -> Result<Memory, MemoryError> {
        let mut memory = self.read(scope, id).await?;
        memory.status = MemoryStatus::Stable;
        memory.verified.push(reviewer);
        self.write(&memory).await?;
        Ok(memory)
    }

    pub fn catalog(&self) -> Option<Catalog> {
        let guard = self.read_index();
        let entries: Vec<&IndexEntry> = guard.values().collect();
        render_refs(&entries)
    }

    pub fn path(&self, scope: MemoryScope, id: &MemoryId) -> Result<PathBuf, MemoryError> {
        Ok(memory_path(self.roots.dir(scope)?, id))
    }

    pub fn contains(&self, scope: MemoryScope, id: &MemoryId) -> bool {
        self.read_index().contains_key(&(scope, id.clone()))
    }

    /// The layers a memory id is indexed in, in layer order.
    pub fn scopes_of(&self, id: &MemoryId) -> Vec<MemoryScope> {
        let guard = self.read_index();
        MemoryScope::ALL
            .into_iter()
            .filter(|scope| guard.contains_key(&(*scope, id.clone())))
            .collect()
    }

    pub fn entries(&self) -> Vec<IndexEntry> {
        self.read_index().values().cloned().collect()
    }

    async fn write(&self, memory: &Memory) -> Result<(), MemoryError> {
        let path = self.path(memory.scope, &memory.id)?;
        let rendered = render(memory)?;
        if let Err(err) = oven_host::write_atomic(&path, rendered).await {
            warn!(path = %path.display(), error = %err, "memory write failed");
            return Err(err.into());
        }
        let modified = fs::metadata(&path)
            .await
            .ok()
            .and_then(|meta| meta.modified().ok())
            .unwrap_or_else(SystemTime::now);
        let stale = stale_reason(memory, &self.roots.workspace).await;
        let entry = entry_of(memory, modified, stale);
        self.write_index()
            .insert((memory.scope, memory.id.clone()), entry);
        Ok(())
    }

    fn read_index(&self) -> RwLockReadGuard<'_, Index> {
        self.index
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write_index(&self) -> RwLockWriteGuard<'_, Index> {
        self.index
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Trims what a writer sent and enforces every limit a memory has.
fn validate(memory: &mut Memory) -> Result<(), MemoryError> {
    memory.description = memory.description.trim().to_owned();
    if memory.description.is_empty() {
        return Err(MemoryError::MissingDescription);
    }
    if memory.description.chars().count() > MAX_DESCRIPTION {
        return Err(MemoryError::DescriptionTooLong);
    }
    memory.why = memory
        .why
        .take()
        .map(|why| why.trim().to_owned())
        .filter(|why| !why.is_empty());
    if memory
        .why
        .as_ref()
        .is_some_and(|why| why.chars().count() > MAX_WHY)
    {
        return Err(MemoryError::WhyTooLong);
    }
    match memory.kind {
        MemoryKind::Preference if !memory.body.trim().is_empty() => {
            return Err(MemoryError::PreferenceBody);
        }
        MemoryKind::Preference => memory.body.clear(),
        MemoryKind::Fact if memory.body.chars().count() > MAX_BODY => {
            return Err(MemoryError::BodyTooLong);
        }
        MemoryKind::Fact => {}
    }
    if memory.anchors.len() > MAX_ANCHORS {
        return Err(MemoryError::TooManyAnchors);
    }
    if let Some(anchor) = memory.anchors.iter().find(|anchor| !is_anchor(anchor)) {
        return Err(MemoryError::InvalidAnchor {
            anchor: anchor.clone(),
        });
    }
    Ok(())
}

/// A workspace-relative path that cannot climb out of the workspace.
fn is_anchor(anchor: &str) -> bool {
    !anchor.trim().is_empty()
        && anchor.chars().count() <= MAX_ANCHOR
        && Path::new(anchor)
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
}

/// The first signal that says a memory is no longer true.
async fn stale_reason(memory: &Memory, workspace: &Path) -> Option<String> {
    if let Some(stale_after) = &memory.stale_after
        && let Ok(instant) = DateTime::parse_from_rfc3339(stale_after)
        && Utc::now() >= instant
    {
        return Some(format!("{PAST_STALE_AFTER} {stale_after}"));
    }
    if !memory.scope.is_local() {
        return None;
    }
    for anchor in &memory.anchors {
        let present = is_anchor(anchor)
            && fs::try_exists(workspace.join(anchor))
                .await
                .unwrap_or(false);
        if !present {
            return Some(format!("{anchor} {GONE}"));
        }
    }
    None
}

fn entry_of(memory: &Memory, modified: SystemTime, stale: Option<String>) -> IndexEntry {
    IndexEntry {
        scope: memory.scope,
        id: memory.id.clone(),
        kind: memory.kind,
        description: memory.description.clone(),
        status: memory.status,
        reviewed: memory.is_reviewed(),
        stale,
        modified,
    }
}

async fn trash(path: &Path, id: &MemoryId) -> io::Result<()> {
    fs::metadata(path).await?;
    let Some(dir) = path.parent() else {
        return Err(io::Error::from(io::ErrorKind::NotFound));
    };
    let trash = dir.join(TRASH_DIR);
    fs::create_dir_all(&trash).await?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    fs::rename(path, trash.join(format!("{id}.{stamp}.{MARKDOWN_EXT}"))).await
}

/// The index of one project layer outside this store, for reading what
/// another project remembers. Anchors are not checked: they belong to a
/// workspace that is not open.
pub async fn layer_entries(dir: &Path) -> Vec<IndexEntry> {
    let mut index = BTreeMap::new();
    let mut skipped = Vec::new();
    load_root(&mut index, &mut skipped, dir, MemoryScope::Project, dir).await;
    index
        .into_values()
        .map(|entry| IndexEntry {
            stale: None,
            ..entry
        })
        .collect()
}

async fn load_root(
    index: &mut Index,
    skipped: &mut Vec<SkippedFile>,
    dir: &Path,
    scope: MemoryScope,
    workspace: &Path,
) {
    let mut entries = match fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return,
        Err(err) => {
            warn!(path = %dir.display(), error = %err, "memory directory unreadable");
            return;
        }
    };
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(err) => {
                warn!(path = %dir.display(), error = %err, "memory directory entry unreadable");
                continue;
            }
        };
        let path = entry.path();
        let id = match markdown_id(&path) {
            None => continue,
            Some(Ok(id)) => id,
            Some(Err(reason)) => {
                skip(skipped, scope, &path, reason);
                continue;
            }
        };
        let meta = match fs::metadata(&path).await {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) => continue,
            Err(err) => {
                skip(skipped, scope, &path, format!("unreadable: {err}"));
                continue;
            }
        };
        let raw = match fs::read_to_string(&path).await {
            Ok(raw) => raw,
            Err(err) => {
                skip(skipped, scope, &path, format!("unreadable: {err}"));
                continue;
            }
        };
        let memory = match parse(id, scope, &raw) {
            Ok(memory) => memory,
            Err(err) => {
                skip(skipped, scope, &path, err.to_string());
                continue;
            }
        };
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let stale = stale_reason(&memory, workspace).await;
        index.insert(
            (scope, memory.id.clone()),
            entry_of(&memory, modified, stale),
        );
    }
}

fn skip(skipped: &mut Vec<SkippedFile>, scope: MemoryScope, path: &Path, reason: String) {
    warn!(path = %path.display(), reason = %reason, "skipping memory file");
    skipped.push(SkippedFile {
        scope,
        file: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        reason,
    });
}

fn memory_path(root: &Path, id: &MemoryId) -> PathBuf {
    root.join(format!("{}.{MARKDOWN_EXT}", id.as_str()))
}

/// The id a memory file is named by. `None` for anything that is not a
/// memory file at all (other extensions, OKF's reserved names); an error
/// for a `.md` file whose name is not a valid id.
fn markdown_id(path: &Path) -> Option<Result<MemoryId, String>> {
    if path.extension().and_then(|ext| ext.to_str()) != Some(MARKDOWN_EXT) {
        return None;
    }
    let stem = path.file_stem().and_then(|stem| stem.to_str())?;
    if crate::model::RESERVED_IDS.contains(&stem) {
        return None;
    }
    Some(MemoryId::new(stem).map_err(|err| format!("file name is not a memory id: {err}")))
}

#[cfg(test)]
#[path = "store_test.rs"]
mod store_test;

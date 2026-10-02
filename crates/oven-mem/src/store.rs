use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

use tokio::fs;
use tracing::warn;

use crate::catalog::{IndexEntry, render_catalog, within_catalog_budget};
use crate::error::MemoryError;
use crate::format::{parse, render};
use crate::model::{MAX_BODY, MAX_DESCRIPTION, Memory, MemoryId, MemoryScope};

const MARKDOWN_EXT: &str = "md";

pub struct MemoryRoots {
    pub workspace: PathBuf,
    pub user: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutOutcome {
    Created,
    Replaced { previous_description: String },
}

pub struct MemoryStore {
    roots: MemoryRoots,
    index: RwLock<BTreeMap<(MemoryScope, MemoryId), IndexEntry>>,
}

impl MemoryStore {
    pub async fn load(roots: MemoryRoots) -> Self {
        let mut index = BTreeMap::new();
        load_root(&mut index, &roots.workspace, MemoryScope::Workspace).await;
        if let Some(user) = &roots.user {
            load_root(&mut index, user, MemoryScope::User).await;
        }
        Self {
            roots,
            index: RwLock::new(index),
        }
    }

    pub async fn read(&self, scope: MemoryScope, id: &MemoryId) -> Result<Memory, MemoryError> {
        let path = memory_path(self.scope_root(scope)?, id);
        let raw = match fs::read_to_string(&path).await {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(MemoryError::NotFound {
                    scope,
                    id: id.clone(),
                });
            }
            Err(err) => {
                return Err(MemoryError::Io {
                    message: err.to_string(),
                });
            }
        };
        parse(id.clone(), scope, &raw)
    }

    pub async fn put(&self, mut memory: Memory) -> Result<PutOutcome, MemoryError> {
        memory.description = memory.description.trim().to_owned();
        if memory.description.is_empty() {
            return Err(MemoryError::MissingDescription);
        }
        if memory.description.chars().count() > MAX_DESCRIPTION {
            return Err(MemoryError::DescriptionTooLong);
        }
        if memory.body.chars().count() > MAX_BODY {
            return Err(MemoryError::BodyTooLong);
        }
        let root = self.scope_root(memory.scope)?.to_path_buf();
        let path = memory_path(&root, &memory.id);
        let outcome = match fs::read_to_string(&path).await {
            Ok(raw) => {
                let previous_description = parse(memory.id.clone(), memory.scope, &raw)
                    .map(|old| old.description)
                    .unwrap_or_default();
                PutOutcome::Replaced {
                    previous_description,
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => PutOutcome::Created,
            Err(err) => {
                return Err(MemoryError::Io {
                    message: err.to_string(),
                });
            }
        };
        if !self.budget_allows(&memory) {
            return Err(MemoryError::CatalogTooLong);
        }
        if let Err(err) = oven_host::write_atomic(&path, render(&memory)).await {
            warn!(path = %path.display(), error = %err, "memory write failed");
            return Err(MemoryError::Io {
                message: err.to_string(),
            });
        }
        let modified = fs::metadata(&path)
            .await
            .ok()
            .and_then(|meta| meta.modified().ok())
            .unwrap_or_else(SystemTime::now);
        self.upsert(&memory, modified);
        Ok(outcome)
    }

    pub fn catalog(&self) -> Option<String> {
        let entries = self.entries();
        render_catalog(&entries)
    }

    fn entries(&self) -> Vec<IndexEntry> {
        let guard = self
            .index
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.values().cloned().collect()
    }

    fn budget_allows(&self, memory: &Memory) -> bool {
        let mut entries = self.entries();
        entries.retain(|entry| !(entry.scope == memory.scope && entry.id == memory.id));
        entries.push(IndexEntry {
            scope: memory.scope,
            id: memory.id.clone(),
            kind: memory.kind,
            description: memory.description.clone(),
            modified: SystemTime::now(),
        });
        within_catalog_budget(&entries)
    }

    fn upsert(&self, memory: &Memory, modified: SystemTime) {
        let mut guard = self
            .index
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.insert(
            (memory.scope, memory.id.clone()),
            IndexEntry {
                scope: memory.scope,
                id: memory.id.clone(),
                kind: memory.kind,
                description: memory.description.clone(),
                modified,
            },
        );
    }

    fn scope_root(&self, scope: MemoryScope) -> Result<&Path, MemoryError> {
        match scope {
            MemoryScope::Workspace => Ok(&self.roots.workspace),
            MemoryScope::User => self
                .roots
                .user
                .as_deref()
                .ok_or(MemoryError::UserScopeUnavailable),
        }
    }
}

async fn load_root(
    index: &mut BTreeMap<(MemoryScope, MemoryId), IndexEntry>,
    dir: &Path,
    scope: MemoryScope,
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
        let Some(id) = markdown_id(&path) else {
            continue;
        };
        let meta = match fs::metadata(&path).await {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) => continue,
            Err(err) => {
                warn!(path = %path.display(), error = %err, "skipping unreadable memory file");
                continue;
            }
        };
        let raw = match fs::read_to_string(&path).await {
            Ok(raw) => raw,
            Err(err) => {
                warn!(path = %path.display(), error = %err, "skipping unreadable memory file");
                continue;
            }
        };
        let memory = match parse(id, scope, &raw) {
            Ok(memory) => memory,
            Err(err) => {
                warn!(path = %path.display(), error = %err, "skipping unparsable memory");
                continue;
            }
        };
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        index.insert(
            (scope, memory.id.clone()),
            IndexEntry {
                scope,
                id: memory.id,
                kind: memory.kind,
                description: memory.description,
                modified,
            },
        );
    }
}

fn memory_path(root: &Path, id: &MemoryId) -> PathBuf {
    root.join(format!("{}.{MARKDOWN_EXT}", id.as_str()))
}

fn markdown_id(path: &Path) -> Option<MemoryId> {
    if path.extension().and_then(|ext| ext.to_str()) != Some(MARKDOWN_EXT) {
        return None;
    }
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        warn!(path = %path.display(), "skipping memory file with invalid id");
        return None;
    };
    match MemoryId::new(stem) {
        Ok(id) => Some(id),
        Err(err) => {
            warn!(path = %path.display(), error = %err, "skipping memory file with invalid id");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use super::{MemoryRoots, MemoryStore, PutOutcome};
    use crate::error::{
        BODY_TOO_LONG, CATALOG_TOO_LONG, DESCRIPTION_TOO_LONG, MemoryError, NOT_FOUND,
        USER_SCOPE_UNAVAILABLE,
    };
    use crate::model::{
        MAX_BODY, MAX_CATALOG_CHARS, MAX_DESCRIPTION, Memory, MemoryId, MemoryKind, MemoryScope,
    };

    const BODY_TOKEN: &str = "BODY-NOT-IN-CATALOG";

    fn memory_file(kind: MemoryKind, description: &str) -> String {
        format!(
            "---\nkind: {}\ndescription: {description}\n---\n\n{BODY_TOKEN}\n",
            kind.as_str()
        )
    }

    fn write_memory(dir: &Path, id: &str, kind: MemoryKind, description: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{id}.md"));
        std::fs::write(&path, memory_file(kind, description)).unwrap();
        path
    }

    #[tokio::test]
    async fn loads_both_scopes_from_their_roots() {
        let tmp = tempdir::TempDir::new("mem-scopes").unwrap();
        let workspace = tmp.path().join("workspace");
        let user = tmp.path().join("user");
        write_memory(
            &workspace,
            "proxy-requires-http2",
            MemoryKind::Fact,
            "proxy fact",
        );
        write_memory(
            &user,
            "clippy-before-done",
            MemoryKind::Preference,
            "run clippy",
        );
        let store = MemoryStore::load(MemoryRoots {
            workspace,
            user: Some(user),
        })
        .await;
        let catalog = store.catalog().unwrap();
        assert!(catalog.contains("- workspace/proxy-requires-http2 proxy fact"));
        assert!(catalog.contains("- user/clippy-before-done run clippy"));
        assert!(!catalog.contains(BODY_TOKEN));
    }

    #[tokio::test]
    async fn skips_invalid_filename() {
        let tmp = tempdir::TempDir::new("mem-bad-name").unwrap();
        let workspace = tmp.path().join("workspace");
        write_memory(&workspace, "ok", MemoryKind::Fact, "kept");
        std::fs::write(
            workspace.join("Bad.md"),
            memory_file(MemoryKind::Fact, "rejected"),
        )
        .unwrap();
        let store = MemoryStore::load(MemoryRoots {
            workspace,
            user: None,
        })
        .await;
        let catalog = store.catalog().unwrap();
        assert!(catalog.contains("- workspace/ok kept"));
        assert!(!catalog.contains("rejected"));
        assert!(!catalog.contains("Bad"));
    }

    #[tokio::test]
    async fn skips_unparsable_file() {
        let tmp = tempdir::TempDir::new("mem-unparsable").unwrap();
        let workspace = tmp.path().join("workspace");
        write_memory(&workspace, "ok", MemoryKind::Fact, "kept");
        std::fs::write(workspace.join("broken.md"), "no frontmatter").unwrap();
        let store = MemoryStore::load(MemoryRoots {
            workspace,
            user: None,
        })
        .await;
        let catalog = store.catalog().unwrap();
        assert!(catalog.contains("- workspace/ok kept"));
        assert!(!catalog.contains("broken"));
        assert!(!catalog.contains("no frontmatter"));
    }

    #[tokio::test]
    async fn ignores_temp_non_md_and_nested_files() {
        let tmp = tempdir::TempDir::new("mem-ignore").unwrap();
        let workspace = tmp.path().join("workspace");
        write_memory(&workspace, "ok", MemoryKind::Fact, "kept");
        std::fs::write(
            workspace.join(".ok.md.tmp-1-1"),
            memory_file(MemoryKind::Fact, "temp file"),
        )
        .unwrap();
        std::fs::write(
            workspace.join("notes.txt"),
            memory_file(MemoryKind::Fact, "plain text"),
        )
        .unwrap();
        let nested = workspace.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("hidden-fact.md"),
            memory_file(MemoryKind::Fact, "nested fact"),
        )
        .unwrap();
        let store = MemoryStore::load(MemoryRoots {
            workspace,
            user: None,
        })
        .await;
        let catalog = store.catalog().unwrap();
        assert!(catalog.contains("- workspace/ok kept"));
        assert!(!catalog.contains("temp file"));
        assert!(!catalog.contains("plain text"));
        assert!(!catalog.contains("nested fact"));
    }

    #[tokio::test]
    async fn missing_directory_is_empty() {
        let tmp = tempdir::TempDir::new("mem-missing").unwrap();
        let store = MemoryStore::load(MemoryRoots {
            workspace: tmp.path().join("missing"),
            user: Some(tmp.path().join("also-missing")),
        })
        .await;
        assert!(store.catalog().is_none());
    }

    #[tokio::test]
    async fn read_sees_a_body_edited_after_load() {
        let tmp = tempdir::TempDir::new("mem-read-edit").unwrap();
        let workspace = tmp.path().join("workspace");
        let path = write_memory(
            &workspace,
            "proxy-requires-http2",
            MemoryKind::Fact,
            "proxy fact",
        );
        let store = MemoryStore::load(MemoryRoots {
            workspace,
            user: None,
        })
        .await;
        std::fs::write(
            &path,
            "---\nkind: fact\ndescription: proxy fact\n---\n\nedited body\n",
        )
        .unwrap();
        let memory = store
            .read(
                MemoryScope::Workspace,
                &MemoryId::new("proxy-requires-http2").unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(memory.body, "edited body\n");
        assert_eq!(memory.description, "proxy fact");
    }

    #[tokio::test]
    async fn read_unknown_id_is_not_found() {
        let tmp = tempdir::TempDir::new("mem-read-missing").unwrap();
        let store = MemoryStore::load(MemoryRoots {
            workspace: tmp.path().join("workspace"),
            user: None,
        })
        .await;
        let id = MemoryId::new("missing").unwrap();
        let err = store.read(MemoryScope::Workspace, &id).await.unwrap_err();
        assert_eq!(
            err,
            MemoryError::NotFound {
                scope: MemoryScope::Workspace,
                id: id.clone(),
            }
        );
        assert_eq!(err.to_string(), format!("{NOT_FOUND}: workspace/{id}"));
    }

    #[tokio::test]
    async fn read_user_scope_without_a_root_is_unavailable() {
        let tmp = tempdir::TempDir::new("mem-read-no-user").unwrap();
        let workspace = tmp.path().join("workspace");
        write_memory(&workspace, "shared-id", MemoryKind::Fact, "workspace only");
        let store = MemoryStore::load(MemoryRoots {
            workspace,
            user: None,
        })
        .await;
        let err = store
            .read(MemoryScope::User, &MemoryId::new("shared-id").unwrap())
            .await
            .unwrap_err();
        assert_eq!(err, MemoryError::UserScopeUnavailable);
        assert_eq!(err.to_string(), USER_SCOPE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn orders_by_mtime_without_sleeping() {
        let tmp = tempdir::TempDir::new("mem-mtime").unwrap();
        let workspace = tmp.path().join("workspace");
        let older = write_memory(&workspace, "older", MemoryKind::Fact, "old fact");
        let newer = write_memory(&workspace, "newer", MemoryKind::Fact, "new fact");
        File::open(&older)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(10))
            .unwrap();
        File::open(&newer)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(20))
            .unwrap();
        let store = MemoryStore::load(MemoryRoots {
            workspace,
            user: None,
        })
        .await;
        let catalog = store.catalog().unwrap();
        let new_at = catalog.find("- workspace/newer new fact").unwrap();
        let old_at = catalog.find("- workspace/older old fact").unwrap();
        assert!(new_at < old_at);
    }

    fn sample(id: &str, description: &str, body: &str, source: Option<&str>) -> Memory {
        Memory {
            id: MemoryId::new(id).unwrap(),
            kind: MemoryKind::Fact,
            description: description.to_owned(),
            body: body.to_owned(),
            scope: MemoryScope::Workspace,
            source: source.map(str::to_owned),
        }
    }

    async fn workspace_store(name: &str) -> (tempdir::TempDir, PathBuf, MemoryStore) {
        let tmp = tempdir::TempDir::new(name).unwrap();
        let workspace = tmp.path().join("workspace");
        let store = MemoryStore::load(MemoryRoots {
            workspace: workspace.clone(),
            user: None,
        })
        .await;
        (tmp, workspace, store)
    }

    #[tokio::test]
    async fn put_creates_a_memory() {
        let (_tmp, workspace, store) = workspace_store("mem-put-create").await;
        let outcome = store
            .put(sample("proxy-requires-http2", "proxy fact", "body\n", None))
            .await
            .unwrap();
        assert_eq!(outcome, PutOutcome::Created);
        assert!(workspace.join("proxy-requires-http2.md").is_file());
        assert!(
            store
                .catalog()
                .unwrap()
                .contains("- workspace/proxy-requires-http2 proxy fact")
        );
    }

    #[tokio::test]
    async fn put_replaced_quotes_a_file_written_behind_the_store() {
        let (_tmp, workspace, store) = workspace_store("mem-put-replace").await;
        store
            .put(sample(
                "proxy-requires-http2",
                "indexed fact",
                "body\n",
                None,
            ))
            .await
            .unwrap();
        std::fs::write(
            workspace.join("proxy-requires-http2.md"),
            "---\nkind: fact\ndescription: from disk\n---\n\nsecret\n",
        )
        .unwrap();
        let outcome = store
            .put(sample(
                "proxy-requires-http2",
                "revised fact",
                "new body\n",
                None,
            ))
            .await
            .unwrap();
        assert_eq!(
            outcome,
            PutOutcome::Replaced {
                previous_description: "from disk".into(),
            }
        );
    }

    #[tokio::test]
    async fn put_rejects_each_limit_by_name() {
        let (_tmp, workspace, store) = workspace_store("mem-put-limits").await;
        let long_description = "d".repeat(MAX_DESCRIPTION + 1);
        let err = store
            .put(sample("too-long", &long_description, "body", None))
            .await
            .unwrap_err();
        assert_eq!(err, MemoryError::DescriptionTooLong);
        assert_eq!(
            err.to_string(),
            format!("{DESCRIPTION_TOO_LONG} ({MAX_DESCRIPTION})")
        );
        assert!(!workspace.join("too-long.md").exists());

        let long_body = "b".repeat(MAX_BODY + 1);
        let err = store
            .put(sample("too-long", "ok", &long_body, None))
            .await
            .unwrap_err();
        assert_eq!(err, MemoryError::BodyTooLong);
        assert_eq!(err.to_string(), format!("{BODY_TOO_LONG} ({MAX_BODY})"));
        assert!(store.catalog().is_none());
    }

    #[tokio::test]
    async fn put_budget_rejection_leaves_the_index_unchanged() {
        let (_tmp, workspace, store) = workspace_store("mem-put-budget").await;
        let description = "d".repeat(MAX_DESCRIPTION);
        let mut n = 0usize;
        let before = loop {
            let id = format!("m{n:03}");
            let before = store.catalog();
            let err = store
                .put(sample(&id, &description, "body", None))
                .await
                .err();
            if let Some(err) = err {
                assert_eq!(err, MemoryError::CatalogTooLong);
                assert_eq!(
                    err.to_string(),
                    format!("{CATALOG_TOO_LONG} ({MAX_CATALOG_CHARS})")
                );
                assert!(!workspace.join(format!("{id}.md")).exists());
                break before;
            }
            n += 1;
            assert!(n < 200, "catalog budget was never reached");
        };
        assert_eq!(store.catalog(), before);
        assert!(n > 0, "the first memory should fit");
    }

    #[tokio::test]
    async fn put_disk_failure_leaves_the_index_unchanged() {
        let (_tmp, workspace, store) = workspace_store("mem-put-disk").await;
        store
            .put(sample("kept", "kept fact", "body\n", None))
            .await
            .unwrap();
        let before = store.catalog();
        std::fs::remove_dir_all(&workspace).unwrap();
        std::fs::write(&workspace, "not a directory").unwrap();
        let err = store
            .put(sample("other", "other fact", "body\n", None))
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::Io { .. }));
        assert_eq!(store.catalog(), before);
        assert!(before.unwrap().contains("kept fact"));
    }

    #[tokio::test]
    async fn put_persists_source() {
        let (_tmp, _workspace, store) = workspace_store("mem-put-source").await;
        store
            .put(sample(
                "proxy-requires-http2",
                "proxy fact",
                "body\n",
                Some("01J8Z"),
            ))
            .await
            .unwrap();
        let memory = store
            .read(
                MemoryScope::Workspace,
                &MemoryId::new("proxy-requires-http2").unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(memory.source.as_deref(), Some("01J8Z"));
    }
}

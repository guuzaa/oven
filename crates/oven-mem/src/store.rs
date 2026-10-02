use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

use tokio::fs;
use tracing::warn;

use crate::catalog::{IndexEntry, render_catalog};
use crate::format::parse;
use crate::model::{MemoryId, MemoryScope};

const MARKDOWN_EXT: &str = "md";

pub struct MemoryRoots {
    pub workspace: PathBuf,
    pub user: Option<PathBuf>,
}

pub struct MemoryStore {
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
            index: RwLock::new(index),
        }
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

    use super::{MemoryRoots, MemoryStore};
    use crate::model::MemoryKind;

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
}

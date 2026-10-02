//! Human-facing memory operations shared by `/memory` and `oven mem`.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use oven_mem::MemoryStore;
use oven_mem::{Memory, MemoryError, MemoryId, MemoryScope, NOT_FOUND};

use crate::core::error::AppError;

pub(crate) const NO_MEMORIES: &str = "no memories";
pub(crate) const MEMORY_DISABLED: &str = "memory is disabled";
pub(crate) const USAGE: &str = "usage: /memory [show <ref> | rm <ref>]";
pub(crate) const AMBIGUOUS_MEMORY: &str = "memory id is in both scopes; qualify it";
pub(crate) const REMOVED_MEMORY: &str = "removed memory";
pub(crate) const UNKNOWN_SCOPE: &str = "unknown memory scope";
pub(crate) const KIND_LABEL: &str = "kind";
pub(crate) const DESCRIPTION_LABEL: &str = "description";
pub(crate) const SOURCE_LABEL: &str = "source";
pub(crate) const NO_EDITOR: &str = "neither VISUAL nor EDITOR is set";

const SHOW: &str = "show";
const RM: &str = "rm";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemoryAction {
    List,
    Show(MemoryRef),
    Remove(MemoryRef),
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
        [SHOW, raw] => Ok(MemoryAction::Show(parse_ref(raw)?)),
        [RM, raw] => Ok(MemoryAction::Remove(parse_ref(raw)?)),
        _ => Err(AppError::Runtime(USAGE.to_owned())),
    }
}

pub(crate) fn parse_ref(raw: &str) -> Result<MemoryRef, AppError> {
    match raw.split_once('/') {
        Some((scope, id)) => Ok(MemoryRef::Scoped {
            scope: parse_scope(scope)?,
            id: parse_id(id)?,
        }),
        None => Ok(MemoryRef::Bare(parse_id(raw)?)),
    }
}

pub async fn open(root: &Path) -> Arc<MemoryStore> {
    Arc::new(MemoryStore::load(crate::dirs::memory_roots(root)).await)
}

pub async fn list(store: &MemoryStore) -> String {
    apply(store, MemoryAction::List).await
}

pub async fn show(store: &MemoryStore, raw: &str) -> Result<String, AppError> {
    Ok(apply(store, MemoryAction::Show(parse_ref(raw)?)).await)
}

pub async fn remove(store: &MemoryStore, raw: &str) -> Result<String, AppError> {
    Ok(apply(store, MemoryAction::Remove(parse_ref(raw)?)).await)
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
    match action {
        MemoryAction::List => render_list(store),
        MemoryAction::Show(memory_ref) => show_ref(store, &memory_ref).await,
        MemoryAction::Remove(memory_ref) => remove_ref(store, &memory_ref).await,
    }
}

fn parse_scope(raw: &str) -> Result<MemoryScope, AppError> {
    if raw == MemoryScope::Workspace.as_str() {
        return Ok(MemoryScope::Workspace);
    }
    if raw == MemoryScope::User.as_str() {
        return Ok(MemoryScope::User);
    }
    Err(AppError::Runtime(format!("{UNKNOWN_SCOPE} '{raw}'")))
}

fn parse_id(raw: &str) -> Result<MemoryId, AppError> {
    MemoryId::new(raw).map_err(|err| AppError::Runtime(err.to_string()))
}

fn render_list(store: &MemoryStore) -> String {
    let mut entries = store.entries();
    if entries.is_empty() {
        return NO_MEMORIES.to_owned();
    }
    entries.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then(left.scope.cmp(&right.scope))
            .then(left.id.cmp(&right.id))
    });
    let mut out = String::new();
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let _ = write!(
            out,
            "{}/{} ({}) {}",
            entry.scope,
            entry.id,
            entry.kind.as_str(),
            entry.description
        );
    }
    out
}

async fn show_ref(store: &MemoryStore, memory_ref: &MemoryRef) -> String {
    match load(store, memory_ref).await {
        Ok(memory) => render_show(&memory),
        Err(text) => text,
    }
}

async fn remove_ref(store: &MemoryStore, memory_ref: &MemoryRef) -> String {
    let (scope, id) = match resolve(store, memory_ref) {
        Ok(found) => found,
        Err(text) => return text,
    };
    match store.forget(scope, &id).await {
        Ok(()) => format!("{REMOVED_MEMORY}: {scope}/{id}"),
        Err(err) => err.to_string(),
    }
}

async fn load(store: &MemoryStore, memory_ref: &MemoryRef) -> Result<Memory, String> {
    let (scope, id) = resolve(store, memory_ref)?;
    store.read(scope, &id).await.map_err(|err| err.to_string())
}

fn resolve(store: &MemoryStore, memory_ref: &MemoryRef) -> Result<(MemoryScope, MemoryId), String> {
    match memory_ref {
        MemoryRef::Scoped { scope, id } => {
            if store
                .entries()
                .iter()
                .any(|entry| entry.scope == *scope && &entry.id == id)
            {
                Ok((*scope, id.clone()))
            } else {
                Err(MemoryError::NotFound {
                    scope: *scope,
                    id: id.clone(),
                }
                .to_string())
            }
        }
        MemoryRef::Bare(id) => locate_bare(store, id),
    }
}

fn locate_bare(store: &MemoryStore, id: &MemoryId) -> Result<(MemoryScope, MemoryId), String> {
    let hits: Vec<_> = store
        .entries()
        .into_iter()
        .filter(|entry| &entry.id == id)
        .collect();
    match hits.len() {
        0 => Err(format!("{NOT_FOUND}: {id}")),
        1 => Ok((hits[0].scope, hits[0].id.clone())),
        _ => Err(format!(
            "{AMBIGUOUS_MEMORY}: {}/{id} or {}/{id}",
            MemoryScope::Workspace,
            MemoryScope::User
        )),
    }
}

fn render_show(memory: &Memory) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{KIND_LABEL}: {}", memory.kind.as_str());
    let _ = writeln!(out, "{DESCRIPTION_LABEL}: {}", memory.description);
    if let Some(source) = &memory.source {
        let _ = writeln!(out, "{SOURCE_LABEL}: {source}");
    }
    out.push('\n');
    out.push_str(&memory.body);
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use oven_mem::{
        EMPTY_ID, INVALID_ID, MemoryId, MemoryKind, MemoryRoots, MemoryScope, MemoryStore,
    };

    use super::*;

    const PROXY_ID: &str = "proxy-requires-http2";
    const PROXY_DESCRIPTION: &str = "The internal proxy only speaks HTTP/2.";
    const PROXY_BODY: &str = "handshake resets\n";
    const SOURCE: &str = "01J8Z";
    const PREF_ID: &str = "clippy-before-done";
    const PREF_DESCRIPTION: &str = "Run cargo clippy before saying done.";

    fn id(raw: &str) -> MemoryId {
        MemoryId::new(raw).unwrap()
    }

    fn scoped(scope: MemoryScope, raw: &str) -> MemoryRef {
        MemoryRef::Scoped { scope, id: id(raw) }
    }

    #[test]
    fn parses_each_form() {
        assert_eq!(parse_command("").unwrap(), MemoryAction::List);
        assert_eq!(parse_command("   ").unwrap(), MemoryAction::List);
        assert_eq!(
            parse_command(&format!("show workspace/{PROXY_ID}")).unwrap(),
            MemoryAction::Show(scoped(MemoryScope::Workspace, PROXY_ID))
        );
        assert_eq!(
            parse_command(&format!("rm user/{PREF_ID}")).unwrap(),
            MemoryAction::Remove(scoped(MemoryScope::User, PREF_ID))
        );
        assert_eq!(
            parse_command(&format!("show {PROXY_ID}")).unwrap(),
            MemoryAction::Show(MemoryRef::Bare(id(PROXY_ID)))
        );
        assert_eq!(
            parse_command(&format!("  rm   {PREF_ID}  ")).unwrap(),
            MemoryAction::Remove(MemoryRef::Bare(id(PREF_ID)))
        );
    }

    #[test]
    fn rejects_invalid_forms() {
        for args in ["nope", "show", "rm", "show one two", "ls"] {
            let err = parse_command(args).unwrap_err();
            assert_eq!(err.to_string(), USAGE, "{args}");
        }
        let err = parse_command("show nope/proxy").unwrap_err();
        assert_eq!(err.to_string(), format!("{UNKNOWN_SCOPE} 'nope'"));
        let err = parse_command("rm ../x").unwrap_err();
        assert_eq!(err.to_string(), format!("{UNKNOWN_SCOPE} '..'"));
        let err = parse_command("show workspace/../x").unwrap_err();
        assert_eq!(err.to_string(), INVALID_ID);
        let err = parse_command("rm A").unwrap_err();
        assert_eq!(err.to_string(), INVALID_ID);
        let err = parse_command("show workspace/").unwrap_err();
        assert_eq!(err.to_string(), EMPTY_ID);
    }

    fn write_memory(
        dir: &Path,
        memory_id: &str,
        kind: MemoryKind,
        description: &str,
        body: &str,
        source: Option<&str>,
        secs: u64,
    ) {
        std::fs::create_dir_all(dir).unwrap();
        let source_line = match source {
            Some(source) => format!("source: {source}\n"),
            None => String::new(),
        };
        let path = dir.join(format!("{memory_id}.md"));
        std::fs::write(
            &path,
            format!(
                "---\nkind: {}\ndescription: {description}\n{source_line}---\n\n{body}",
                kind.as_str()
            ),
        )
        .unwrap();
        // SetFileTime needs FILE_WRITE_ATTRIBUTES; a read-only handle is denied on Windows.
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    async fn loaded(workspace: &Path, user: &Path) -> MemoryStore {
        MemoryStore::load(MemoryRoots {
            workspace: workspace.to_path_buf(),
            user: Some(user.to_path_buf()),
        })
        .await
    }

    #[tokio::test]
    async fn list_is_newest_first_and_empty_when_none() {
        let tmp = tempdir::TempDir::new("memory-list").unwrap();
        let workspace = tmp.path().join("workspace");
        let user = tmp.path().join("user");
        let empty = loaded(&workspace, &user).await;
        assert_eq!(list(&empty).await, NO_MEMORIES);

        write_memory(
            &workspace,
            PROXY_ID,
            MemoryKind::Fact,
            PROXY_DESCRIPTION,
            PROXY_BODY,
            Some(SOURCE),
            10,
        );
        write_memory(
            &user,
            PREF_ID,
            MemoryKind::Preference,
            PREF_DESCRIPTION,
            "always\n",
            None,
            20,
        );
        let store = loaded(&workspace, &user).await;
        assert_eq!(
            list(&store).await,
            format!(
                "user/{PREF_ID} ({}) {PREF_DESCRIPTION}\nworkspace/{PROXY_ID} ({}) {PROXY_DESCRIPTION}",
                MemoryKind::Preference.as_str(),
                MemoryKind::Fact.as_str()
            )
        );
    }

    #[tokio::test]
    async fn show_includes_kind_description_source_and_body() {
        let tmp = tempdir::TempDir::new("memory-show").unwrap();
        let workspace = tmp.path().join("workspace");
        let user = tmp.path().join("user");
        write_memory(
            &workspace,
            PROXY_ID,
            MemoryKind::Fact,
            PROXY_DESCRIPTION,
            PROXY_BODY,
            Some(SOURCE),
            1,
        );
        write_memory(
            &user,
            PREF_ID,
            MemoryKind::Preference,
            PREF_DESCRIPTION,
            "always\n",
            None,
            1,
        );
        let store = loaded(&workspace, &user).await;
        assert_eq!(
            show(&store, &format!("workspace/{PROXY_ID}"))
                .await
                .unwrap(),
            format!(
                "{KIND_LABEL}: {}\n{DESCRIPTION_LABEL}: {PROXY_DESCRIPTION}\n{SOURCE_LABEL}: {SOURCE}\n\n{PROXY_BODY}",
                MemoryKind::Fact.as_str()
            )
        );
        assert_eq!(
            show(&store, PREF_ID).await.unwrap(),
            format!(
                "{KIND_LABEL}: {}\n{DESCRIPTION_LABEL}: {PREF_DESCRIPTION}\n\nalways\n",
                MemoryKind::Preference.as_str()
            )
        );
    }

    #[tokio::test]
    async fn ambiguous_bare_id_asks_to_qualify() {
        let tmp = tempdir::TempDir::new("memory-ambiguous").unwrap();
        let workspace = tmp.path().join("workspace");
        let user = tmp.path().join("user");
        write_memory(
            &workspace,
            PROXY_ID,
            MemoryKind::Fact,
            "workspace copy",
            "ws\n",
            None,
            1,
        );
        write_memory(
            &user,
            PROXY_ID,
            MemoryKind::Fact,
            "user copy",
            "user\n",
            None,
            2,
        );
        let store = loaded(&workspace, &user).await;
        let text = show(&store, PROXY_ID).await.unwrap();
        assert_eq!(
            text,
            format!(
                "{AMBIGUOUS_MEMORY}: {}/{PROXY_ID} or {}/{PROXY_ID}",
                MemoryScope::Workspace,
                MemoryScope::User
            )
        );
    }

    #[tokio::test]
    async fn remove_unknown_is_a_reply_and_known_is_removed() {
        let tmp = tempdir::TempDir::new("memory-rm").unwrap();
        let workspace = tmp.path().join("workspace");
        let user = tmp.path().join("user");
        write_memory(
            &workspace,
            PROXY_ID,
            MemoryKind::Fact,
            PROXY_DESCRIPTION,
            PROXY_BODY,
            None,
            1,
        );
        let store = loaded(&workspace, &user).await;
        let missing = "missing";
        assert_eq!(
            remove(&store, missing).await.unwrap(),
            format!("{NOT_FOUND}: {missing}")
        );
        assert!(workspace.join(format!("{PROXY_ID}.md")).is_file());
        assert_eq!(
            remove(&store, &format!("workspace/{PROXY_ID}"))
                .await
                .unwrap(),
            format!("{REMOVED_MEMORY}: {}/{PROXY_ID}", MemoryScope::Workspace)
        );
        assert!(!workspace.join(format!("{PROXY_ID}.md")).exists());
        assert_eq!(list(&store).await, NO_MEMORIES);
    }

    #[tokio::test]
    async fn edit_path_resolves_a_unique_ref() {
        let tmp = tempdir::TempDir::new("memory-edit-path").unwrap();
        let workspace = tmp.path().join("workspace");
        let user = tmp.path().join("user");
        write_memory(
            &workspace,
            PROXY_ID,
            MemoryKind::Fact,
            PROXY_DESCRIPTION,
            PROXY_BODY,
            None,
            1,
        );
        write_memory(
            &user,
            PREF_ID,
            MemoryKind::Preference,
            PREF_DESCRIPTION,
            "always\n",
            None,
            1,
        );
        let store = loaded(&workspace, &user).await;
        assert_eq!(
            file_path(&store, &format!("workspace/{PROXY_ID}"))
                .await
                .unwrap(),
            workspace.join(format!("{PROXY_ID}.md"))
        );
        assert_eq!(
            file_path(&store, PREF_ID).await.unwrap(),
            user.join(format!("{PREF_ID}.md"))
        );
        let err = file_path(&store, "missing").await.unwrap_err();
        assert_eq!(err.to_string(), format!("{NOT_FOUND}: missing"));
    }

    #[test]
    fn editor_program_prefers_visual_and_errors_when_unset() {
        assert_eq!(editor_program(Some("vim"), Some("nano")).unwrap(), "vim");
        assert_eq!(editor_program(Some("  "), Some(" nano ")).unwrap(), "nano");
        let err = editor_program(None, Some("  ")).unwrap_err();
        assert_eq!(err.to_string(), NO_EDITOR);
        let err = editor_program(None, None).unwrap_err();
        assert_eq!(err.to_string(), NO_EDITOR);
    }
}

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{MemoryRoots, MemoryStore, PutOutcome};
use crate::error::{
    BODY_TOO_LONG, DESCRIPTION_TOO_LONG, INVALID_ANCHOR, LAYER_FULL, LAYER_UNAVAILABLE,
    MemoryError, NOT_FOUND, PREFERENCE_BODY, SHARED_READ_ONLY, TOO_MANY_ANCHORS, WHY_TOO_LONG,
};
use crate::model::{
    GLOBAL_BUDGET, MAX_ANCHOR, MAX_ANCHORS, MAX_BODY, MAX_DESCRIPTION, MAX_WHY, Memory, MemoryId,
    MemoryKind, MemoryScope, MemoryStatus, Stamp,
};

const BODY_TOKEN: &str = "BODY-NOT-IN-CATALOG";
const REVIEWER: &str = "me";

struct Layers {
    _tmp: tempdir::TempDir,
    workspace: PathBuf,
    project: PathBuf,
    shared: PathBuf,
    global: PathBuf,
}

impl Layers {
    fn new(name: &str) -> Self {
        let tmp = tempdir::TempDir::new(name).unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        Self {
            project: tmp.path().join("project"),
            shared: workspace.join(".oven").join("memory"),
            global: tmp.path().join("global"),
            workspace,
            _tmp: tmp,
        }
    }

    fn roots(&self) -> MemoryRoots {
        MemoryRoots {
            workspace: self.workspace.clone(),
            project: Some(self.project.clone()),
            shared: self.shared.clone(),
            global: Some(self.global.clone()),
            inbox: None,
        }
    }

    async fn store(&self) -> MemoryStore {
        MemoryStore::load(self.roots()).await
    }
}

fn file(kind: &str, description: &str, extra: &str) -> String {
    format!("---\ntype: {kind}\ndescription: {description}\n{extra}---\n\n{BODY_TOKEN}\n")
}

fn write_file(dir: &Path, id: &str, raw: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{id}.md"));
    std::fs::write(&path, raw).unwrap();
    path
}

fn id(raw: &str) -> MemoryId {
    MemoryId::new(raw).unwrap()
}

fn fact(scope: MemoryScope, raw_id: &str, description: &str) -> Memory {
    let mut memory = Memory::new(id(raw_id), scope, MemoryKind::Fact, description);
    memory.body = "body\n".into();
    memory
}

fn catalog_text(store: &MemoryStore) -> String {
    store.catalog().map(|c| c.text).unwrap_or_default()
}

#[tokio::test]
async fn loads_each_layer_from_its_own_root() {
    let layers = Layers::new("mem-layers");
    write_file(&layers.project, "proxy", &file("Fact", "proxy fact", ""));
    write_file(
        &layers.shared,
        "small-prs",
        &file("Preference", "small prs", ""),
    );
    write_file(
        &layers.global,
        "clippy",
        &file("Preference", "run clippy", ""),
    );
    let text = catalog_text(&layers.store().await);
    assert!(text.contains("- project/proxy proxy fact"));
    assert!(text.contains("- shared/small-prs small prs"));
    assert!(text.contains("- global/clippy run clippy"));
    assert!(!text.contains(BODY_TOKEN));
}

#[tokio::test]
async fn without_a_home_only_shared_loads_and_writers_name_the_layer() {
    let layers = Layers::new("mem-no-home");
    write_file(
        &layers.shared,
        "small-prs",
        &file("Preference", "small prs", ""),
    );
    let store = MemoryStore::load(MemoryRoots {
        project: None,
        global: None,
        ..layers.roots()
    })
    .await;
    assert!(catalog_text(&store).contains("- shared/small-prs"));
    let err = store
        .put(fact(MemoryScope::Project, "x", "x"))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        MemoryError::LayerUnavailable {
            scope: MemoryScope::Project
        }
    );
    assert_eq!(err.to_string(), format!("{LAYER_UNAVAILABLE}: project"));
}

#[tokio::test]
async fn skips_what_is_not_a_memory() {
    let layers = Layers::new("mem-skips");
    write_file(&layers.project, "ok", &file("Fact", "kept", ""));
    write_file(&layers.project, "Bad", &file("Fact", "bad name", ""));
    write_file(&layers.project, "log", "# Directory Update Log\n");
    write_file(&layers.project, "index", "# Index\n");
    write_file(&layers.project, "broken", "no frontmatter");
    std::fs::write(
        layers.project.join(".ok.md.tmp-1-1"),
        file("Fact", "temp file", ""),
    )
    .unwrap();
    std::fs::write(layers.project.join("notes.txt"), file("Fact", "plain", "")).unwrap();
    write_file(
        &layers.project.join(".trash"),
        "trashed",
        &file("Fact", "trashed fact", ""),
    );
    let store = layers.store().await;
    let ids: Vec<String> = store.entries().iter().map(|e| e.id.to_string()).collect();
    assert_eq!(ids, vec!["ok"]);
    let skipped: Vec<(String, String)> = store
        .skipped()
        .iter()
        .map(|skip| (skip.file.clone(), skip.reason.clone()))
        .collect();
    assert_eq!(
        skipped,
        vec![
            (
                "Bad.md".to_owned(),
                "file name is not a memory id: memory id must match [a-z0-9-]+".to_owned()
            ),
            (
                "broken.md".to_owned(),
                crate::error::MISSING_FRONTMATTER.to_owned()
            ),
        ]
    );
}

#[tokio::test]
async fn missing_directories_are_empty() {
    let layers = Layers::new("mem-missing");
    assert!(layers.store().await.catalog().is_none());
}

#[tokio::test]
async fn read_sees_a_body_edited_after_load() {
    let layers = Layers::new("mem-read-edit");
    let path = write_file(&layers.project, "proxy", &file("Fact", "proxy fact", ""));
    let store = layers.store().await;
    std::fs::write(
        &path,
        "---\ntype: Fact\ndescription: proxy fact\n---\n\nedited\n",
    )
    .unwrap();
    let memory = store
        .read(MemoryScope::Project, &id("proxy"))
        .await
        .unwrap();
    assert_eq!(memory.body, "edited\n");
}

#[tokio::test]
async fn read_unknown_id_is_not_found() {
    let layers = Layers::new("mem-read-missing");
    let store = layers.store().await;
    let err = store
        .read(MemoryScope::Global, &id("missing"))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), format!("{NOT_FOUND}: global/missing"));
}

#[tokio::test]
async fn orders_by_mtime_without_sleeping() {
    let layers = Layers::new("mem-mtime");
    let older = write_file(&layers.project, "older", &file("Fact", "old fact", ""));
    let newer = write_file(&layers.project, "newer", &file("Fact", "new fact", ""));
    // SetFileTime needs FILE_WRITE_ATTRIBUTES; a read-only handle is denied on Windows.
    for (path, secs) in [(&older, 10), (&newer, 20)] {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }
    let text = catalog_text(&layers.store().await);
    assert!(text.find("project/newer").unwrap() < text.find("project/older").unwrap());
}

#[tokio::test]
async fn put_creates_then_replaces_quoting_the_disk() {
    let layers = Layers::new("mem-put");
    let store = layers.store().await;
    let outcome = store
        .put(fact(MemoryScope::Project, "proxy", "indexed"))
        .await
        .unwrap();
    assert_eq!(outcome, PutOutcome::Created);
    assert!(catalog_text(&store).contains("- project/proxy indexed"));
    std::fs::write(
        layers.project.join("proxy.md"),
        "---\ntype: Fact\ndescription: from disk\n---\n",
    )
    .unwrap();
    let outcome = store
        .put(fact(MemoryScope::Project, "proxy", "revised"))
        .await
        .unwrap();
    assert_eq!(
        outcome,
        PutOutcome::Replaced {
            previous_description: "from disk".into()
        }
    );
}

#[tokio::test]
async fn put_rejects_each_limit_by_name() {
    let layers = Layers::new("mem-limits");
    let store = layers.store().await;
    let scope = MemoryScope::Project;

    let long = fact(scope, "x", &"d".repeat(MAX_DESCRIPTION + 1));
    let err = store.put(long).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("{DESCRIPTION_TOO_LONG} ({MAX_DESCRIPTION})")
    );

    let mut why = fact(scope, "x", "ok");
    why.why = Some("w".repeat(MAX_WHY + 1));
    let err = store.put(why).await.unwrap_err();
    assert_eq!(err.to_string(), format!("{WHY_TOO_LONG} ({MAX_WHY})"));

    let mut body = fact(scope, "x", "ok");
    body.body = "b".repeat(MAX_BODY + 1);
    let err = store.put(body).await.unwrap_err();
    assert_eq!(err.to_string(), format!("{BODY_TOO_LONG} ({MAX_BODY})"));

    let mut preference = fact(scope, "x", "ok");
    preference.kind = MemoryKind::Preference;
    let err = store.put(preference).await.unwrap_err();
    assert_eq!(err.to_string(), PREFERENCE_BODY);

    let mut anchors = fact(scope, "x", "ok");
    anchors.anchors = vec!["a.rs".into(); MAX_ANCHORS + 1];
    let err = store.put(anchors).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("{TOO_MANY_ANCHORS} ({MAX_ANCHORS})")
    );

    for bad in ["../secret", "/etc/passwd", ""] {
        let mut anchor = fact(scope, "x", "ok");
        anchor.anchors = vec![bad.into()];
        let err = store.put(anchor).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("{INVALID_ANCHOR} (at most {MAX_ANCHOR} chars): {bad}")
        );
    }

    let err = store
        .put(fact(MemoryScope::Shared, "x", "ok"))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), SHARED_READ_ONLY);

    assert!(store.entries().is_empty());
    assert!(!layers.project.join("x.md").exists());
}

#[tokio::test]
async fn a_preference_body_of_whitespace_is_dropped() {
    let layers = Layers::new("mem-pref-blank");
    let store = layers.store().await;
    let mut preference = Memory::new(id("rule"), MemoryScope::Global, MemoryKind::Preference, "r");
    preference.body = "\n  \n".into();
    store.put(preference).await.unwrap();
    let raw = std::fs::read_to_string(layers.global.join("rule.md")).unwrap();
    assert_eq!(raw, "---\ntype: Preference\ndescription: r\n---\n");
}

#[tokio::test]
async fn each_layer_has_its_own_budget() {
    let layers = Layers::new("mem-budget");
    let store = layers.store().await;
    let description = "d".repeat(MAX_DESCRIPTION);
    let mut n = 0;
    let err = loop {
        let memory = fact(MemoryScope::Global, &format!("g{n:03}"), &description);
        match store.put(memory).await {
            Ok(_) => n += 1,
            Err(err) => break err,
        }
        assert!(n < 100, "the global budget was never reached");
    };
    assert_eq!(
        err.to_string(),
        format!("{LAYER_FULL} (global: {GLOBAL_BUDGET} chars)")
    );
    assert!(n > 0);
    let before = store.catalog();

    let mut draft = fact(MemoryScope::Global, "draft", &description);
    draft.status = MemoryStatus::Draft;
    store.put(draft).await.unwrap();
    assert_eq!(store.catalog(), before);

    store
        .put(fact(MemoryScope::Project, "local", "still fits"))
        .await
        .unwrap();
    assert!(catalog_text(&store).contains("- project/local still fits"));
}

#[tokio::test]
async fn put_disk_failure_leaves_the_index_unchanged() {
    let layers = Layers::new("mem-put-disk");
    let store = layers.store().await;
    store
        .put(fact(MemoryScope::Project, "kept", "kept fact"))
        .await
        .unwrap();
    let before = store.catalog();
    std::fs::remove_dir_all(&layers.project).unwrap();
    std::fs::write(&layers.project, "not a directory").unwrap();
    let err = store
        .put(fact(MemoryScope::Project, "other", "other"))
        .await
        .unwrap_err();
    assert!(matches!(err, MemoryError::Io { .. }));
    assert_eq!(store.catalog(), before);
}

#[tokio::test]
async fn missing_anchors_and_a_past_stale_after_mark_a_fact_stale() {
    let layers = Layers::new("mem-stale");
    std::fs::write(layers.workspace.join("present.rs"), "").unwrap();
    write_file(
        &layers.project,
        "anchored",
        &file("Fact", "anchored", "anchors: [present.rs]\n"),
    );
    write_file(
        &layers.project,
        "moved",
        &file("Fact", "moved", "anchors: [present.rs, gone.rs]\n"),
    );
    write_file(
        &layers.project,
        "expired",
        &file("Fact", "expired", "stale_after: 2000-01-01T00:00:00Z\n"),
    );
    write_file(
        &layers.global,
        "elsewhere",
        &file("Fact", "elsewhere", "anchors: [gone.rs]\n"),
    );
    let text = catalog_text(&layers.store().await);
    assert!(text.contains("- project/anchored anchored\n"));
    assert!(text.contains("- project/moved moved [stale: gone.rs is gone]\n"));
    assert!(
        text.contains("- project/expired expired [stale: past stale_after 2000-01-01T00:00:00Z]\n")
    );
    assert!(text.contains("- global/elsewhere elsewhere\n"));
}

#[tokio::test]
async fn forget_trashes_private_layers_and_deletes_shared() {
    let layers = Layers::new("mem-forget");
    write_file(&layers.project, "local", &file("Fact", "local", ""));
    write_file(&layers.shared, "team", &file("Fact", "team", ""));
    let store = layers.store().await;
    store
        .forget(MemoryScope::Project, &id("local"))
        .await
        .unwrap();
    assert!(!layers.project.join("local.md").exists());
    let trashed: Vec<_> = std::fs::read_dir(layers.project.join(".trash"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(trashed.len(), 1);
    assert!(trashed[0].starts_with("local."), "{trashed:?}");

    store
        .forget(MemoryScope::Shared, &id("team"))
        .await
        .unwrap();
    assert!(!layers.shared.join("team.md").exists());
    assert!(!layers.shared.join(".trash").exists());
    assert!(store.entries().is_empty());

    let err = store
        .forget(MemoryScope::Project, &id("local"))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), format!("{NOT_FOUND}: project/local"));
}

#[tokio::test]
async fn move_to_reviews_and_refuses_to_overwrite() {
    let layers = Layers::new("mem-move");
    write_file(&layers.project, "rule", &file("Preference", "the rule", ""));
    write_file(&layers.global, "taken", &file("Preference", "taken", ""));
    write_file(&layers.project, "taken", &file("Preference", "local", ""));
    let store = layers.store().await;
    let moved = store
        .move_to(
            MemoryScope::Project,
            &id("rule"),
            MemoryScope::Global,
            Stamp::human(REVIEWER),
        )
        .await
        .unwrap();
    assert!(moved.is_reviewed());
    assert!(!layers.project.join("rule.md").exists());
    let reread = store.read(MemoryScope::Global, &id("rule")).await.unwrap();
    assert!(reread.is_reviewed());
    assert!(store.contains(MemoryScope::Global, &id("rule")));
    assert!(!store.contains(MemoryScope::Project, &id("rule")));

    let err = store
        .move_to(
            MemoryScope::Project,
            &id("taken"),
            MemoryScope::Global,
            Stamp::human(REVIEWER),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err,
        MemoryError::AlreadyExists {
            scope: MemoryScope::Global,
            id: id("taken")
        }
    );
    assert!(layers.project.join("taken.md").exists());
}

#[tokio::test]
async fn approve_turns_a_draft_into_reviewed_memory() {
    let layers = Layers::new("mem-approve");
    write_file(
        &layers.global,
        "rule",
        &file("Preference", "the rule", "status: draft\n"),
    );
    let store = layers.store().await;
    assert!(store.catalog().is_none());
    let approved = store
        .approve(MemoryScope::Global, &id("rule"), Stamp::human(REVIEWER))
        .await
        .unwrap();
    assert_eq!(approved.status, MemoryStatus::Stable);
    assert!(approved.is_reviewed());
    assert!(catalog_text(&store).contains("- global/rule the rule"));
    let entry = &store.entries()[0];
    assert!(entry.reviewed);
}

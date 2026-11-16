use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{Change, TidyContext, TidyReport, apply_plan};
use crate::error::{PREFERENCE_BODY, SHARED_READ_ONLY};
use crate::model::{MemoryId, MemoryScope, MemoryStatus, Source};
use crate::store::{MemoryRoots, MemoryStore};
use crate::tidy::plan::parse_plan;

const MODEL: &str = "m1";
const ROOT: &str = "/w/oven";

struct Layers {
    _tmp: tempdir::TempDir,
    base: PathBuf,
}

impl Layers {
    fn new(name: &str) -> Self {
        let tmp = tempdir::TempDir::new(name).unwrap();
        let base = tmp.path().to_path_buf();
        Self { _tmp: tmp, base }
    }

    fn dir(&self, scope: MemoryScope) -> PathBuf {
        self.base.join(scope.as_str())
    }

    fn write(&self, scope: MemoryScope, id: &str, front: &str) {
        write_in(&self.dir(scope), id, front);
    }

    async fn store(&self) -> MemoryStore {
        MemoryStore::load(MemoryRoots {
            workspace: self.base.clone(),
            project: Some(self.dir(MemoryScope::Project)),
            shared: self.dir(MemoryScope::Shared),
            global: Some(self.dir(MemoryScope::Global)),
            inbox: None,
        })
        .await
    }
}

fn write_in(dir: &Path, id: &str, front: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(format!("{id}.md")), format!("---\n{front}---\n")).unwrap();
}

fn id(raw: &str) -> MemoryId {
    MemoryId::new(raw).unwrap()
}

async fn run(
    store: &MemoryStore,
    reply: &str,
    origins: &BTreeMap<MemoryId, PathBuf>,
) -> TidyReport {
    let cx = TidyContext {
        model: MODEL,
        root: ROOT,
        origins,
    };
    apply_plan(store, parse_plan(reply).unwrap(), &cx).await
}

fn skipped(report: &TidyReport) -> Vec<String> {
    report
        .changes
        .iter()
        .filter_map(|change| match change {
            Change::Skipped { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_project_put_is_stable_stamped_and_logged() {
    let layers = Layers::new("tidy-put-project");
    let store = layers.store().await;
    let report = run(
        &store,
        r#"{"ops": [{"op": "put", "scope": "project", "id": "proxy", "type": "fact",
            "description": "proxy is http2", "body": "use prior knowledge\n", "anchors": ["Cargo.toml"]}]}"#,
        &BTreeMap::new(),
    )
    .await;
    assert_eq!(
        report.changes,
        vec![Change::Created {
            reference: "project/proxy".into(),
            description: "proxy is http2".into()
        }]
    );
    let memory = store
        .read(MemoryScope::Project, &id("proxy"))
        .await
        .unwrap();
    assert_eq!(memory.status, MemoryStatus::Stable);
    assert_eq!(memory.generated.unwrap().by, format!("oven/{MODEL}"));
    assert_eq!(memory.sources, vec![Source::project(ROOT)]);
    let log = std::fs::read_to_string(layers.dir(MemoryScope::Project).join("log.md")).unwrap();
    assert!(log.starts_with("# Directory Update Log\n\n## "), "{log}");
    assert!(
        log.contains("* **Creation**: [proxy](/proxy.md) proxy is http2\n"),
        "{log}"
    );
    assert!(
        store
            .catalog()
            .unwrap()
            .text
            .contains("- project/proxy proxy is http2")
    );
}

#[tokio::test]
async fn a_new_global_memory_is_a_draft_that_keeps_what_it_would_replace() {
    let layers = Layers::new("tidy-put-global");
    layers.write(
        MemoryScope::Project,
        "comments",
        "type: Preference\ndescription: local rule\n",
    );
    let store = layers.store().await;
    let report = run(
        &store,
        r#"{"ops": [{"op": "put", "scope": "global", "id": "comments", "type": "preference",
            "description": "say why, not what", "replaces": ["project/comments"]}]}"#,
        &BTreeMap::new(),
    )
    .await;
    assert_eq!(
        report.changes,
        vec![Change::Drafted {
            reference: "global/comments".into(),
            kept: vec!["project/comments".into()]
        }]
    );
    let draft = store
        .read(MemoryScope::Global, &id("comments"))
        .await
        .unwrap();
    assert_eq!(draft.status, MemoryStatus::Draft);
    assert!(store.contains(MemoryScope::Project, &id("comments")));
    let catalog = store.catalog().unwrap().text;
    assert!(catalog.contains("- project/comments local rule"));
    assert!(!catalog.contains("global/comments"));
}

#[tokio::test]
async fn rewriting_an_unreviewed_global_memory_keeps_it_stable() {
    let layers = Layers::new("tidy-rewrite-global");
    layers.write(
        MemoryScope::Global,
        "comments",
        "type: Preference\ndescription: long rule\nsources:\n  - { resource: \"session:s1\" }\ntags: [style]\n",
    );
    let store = layers.store().await;
    let report = run(
        &store,
        r#"{"ops": [{"op": "put", "scope": "global", "id": "comments", "type": "preference",
            "description": "short rule"}]}"#,
        &BTreeMap::new(),
    )
    .await;
    assert_eq!(
        report.changes,
        vec![Change::Updated {
            reference: "global/comments".into()
        }]
    );
    let memory = store
        .read(MemoryScope::Global, &id("comments"))
        .await
        .unwrap();
    assert_eq!(memory.status, MemoryStatus::Stable);
    assert_eq!(memory.description, "short rule");
    assert_eq!(memory.sources.len(), 2, "{:?}", memory.sources);
    assert!(memory.extra.contains_key("tags"));
}

#[tokio::test]
async fn a_merge_removes_what_it_absorbs_and_keeps_their_sources() {
    let layers = Layers::new("tidy-merge");
    layers.write(
        MemoryScope::Project,
        "a",
        "type: Fact\ndescription: a\nsources:\n  - { resource: \"session:sa\" }\n",
    );
    layers.write(
        MemoryScope::Global,
        "b",
        "type: Fact\ndescription: b\nsources:\n  - { resource: \"session:sb\" }\n",
    );
    let store = layers.store().await;
    let report = run(
        &store,
        r#"{"ops": [{"op": "put", "scope": "project", "id": "ab", "type": "fact",
            "description": "a and b", "replaces": ["project/a", "global/b"]}]}"#,
        &BTreeMap::new(),
    )
    .await;
    assert_eq!(
        report.changes,
        vec![Change::Merged {
            reference: "project/ab".into(),
            absorbed: vec!["project/a".into(), "global/b".into()]
        }]
    );
    assert!(!store.contains(MemoryScope::Project, &id("a")));
    assert!(!store.contains(MemoryScope::Global, &id("b")));
    assert!(layers.dir(MemoryScope::Project).join(".trash").is_dir());
    let merged = store.read(MemoryScope::Project, &id("ab")).await.unwrap();
    let resources: Vec<&str> = merged.sources.iter().map(|s| s.resource.as_str()).collect();
    assert_eq!(
        resources,
        vec!["session:sa", "session:sb", "project:/w/oven"]
    );
    let global_log =
        std::fs::read_to_string(layers.dir(MemoryScope::Global).join("log.md")).unwrap();
    assert!(
        global_log.contains("* **Deletion**: b: merged into project/ab\n"),
        "{global_log}"
    );
}

#[tokio::test]
async fn shared_reviewed_and_invalid_targets_are_refused() {
    let layers = Layers::new("tidy-refuse");
    layers.write(
        MemoryScope::Shared,
        "team",
        "type: Fact\ndescription: team\n",
    );
    layers.write(
        MemoryScope::Project,
        "kept",
        "type: Fact\ndescription: kept\nverified: { by: \"human:me\" }\n",
    );
    let store = layers.store().await;
    let report = run(
        &store,
        r#"{"ops": [
            {"op": "put", "scope": "shared", "id": "x", "type": "fact", "description": "x"},
            {"op": "put", "scope": "project", "id": "y", "type": "fact", "description": "y", "replaces": ["shared/team"]},
            {"op": "put", "scope": "project", "id": "kept", "type": "fact", "description": "changed"},
            {"op": "delete", "ref": "project/kept", "reason": "old"},
            {"op": "delete", "ref": "shared/team"},
            {"op": "put", "scope": "project", "id": "rule", "type": "preference", "description": "r", "body": "no"},
            {"op": "put", "scope": "project", "id": "z", "type": "playbook", "description": "z"},
            {"op": "rename", "ref": "project/kept"}
        ]}"#,
        &BTreeMap::new(),
    )
    .await;
    let reasons = skipped(&report);
    assert_eq!(reasons.len(), 8, "{reasons:?}");
    assert_eq!(reasons[0], SHARED_READ_ONLY);
    assert_eq!(reasons[1], SHARED_READ_ONLY);
    assert_eq!(reasons[2], super::REVIEWED_LOCKED);
    assert_eq!(reasons[3], super::REVIEWED_LOCKED);
    assert_eq!(reasons[4], SHARED_READ_ONLY);
    assert_eq!(reasons[5], PREFERENCE_BODY);
    assert_eq!(reasons[6], format!("{} 'playbook'", super::UNKNOWN_TYPE));
    assert!(store.contains(MemoryScope::Shared, &id("team")));
    let kept = store.read(MemoryScope::Project, &id("kept")).await.unwrap();
    assert_eq!(kept.description, "kept");
    assert!(!store.contains(MemoryScope::Project, &id("y")));
}

#[tokio::test]
async fn delete_trashes_and_demote_follows_the_origin() {
    let layers = Layers::new("tidy-demote");
    layers.write(
        MemoryScope::Project,
        "old",
        "type: Fact\ndescription: old\n",
    );
    layers.write(
        MemoryScope::Global,
        "ui",
        "type: Preference\ndescription: web ui rule\n",
    );
    layers.write(
        MemoryScope::Global,
        "here",
        "type: Preference\ndescription: oven rule\n",
    );
    layers.write(
        MemoryScope::Global,
        "lost",
        "type: Preference\ndescription: lost rule\n",
    );
    let elsewhere = layers.base.join("elsewhere");
    let origins = BTreeMap::from([
        (id("ui"), elsewhere.clone()),
        (id("here"), layers.dir(MemoryScope::Project)),
    ]);
    let store = layers.store().await;
    let report = run(
        &store,
        r#"{"ops": [
            {"op": "delete", "ref": "project/old", "reason": "stale"},
            {"op": "demote", "ref": "global/ui"},
            {"op": "demote", "ref": "global/here"},
            {"op": "demote", "ref": "global/lost"},
            {"op": "demote", "ref": "project/here"}
        ]}"#,
        &origins,
    )
    .await;
    assert_eq!(
        report.changes[..3],
        [
            Change::Deleted {
                reference: "project/old".into(),
                reason: "stale".into()
            },
            Change::Demoted {
                reference: "global/ui".into(),
                to: elsewhere.display().to_string()
            },
            Change::Demoted {
                reference: "global/here".into(),
                to: layers.dir(MemoryScope::Project).display().to_string()
            },
        ]
    );
    assert_eq!(
        skipped(&report),
        vec![
            super::ORIGIN_UNKNOWN.to_owned(),
            super::NOT_GLOBAL.to_owned()
        ]
    );
    assert!(elsewhere.join("ui.md").is_file());
    assert!(!store.contains(MemoryScope::Global, &id("ui")));
    assert!(store.contains(MemoryScope::Project, &id("here")));
    assert!(store.contains(MemoryScope::Global, &id("lost")));
    let log = std::fs::read_to_string(elsewhere.join("log.md")).unwrap();
    assert!(
        log.contains("* **Creation**: [ui](/ui.md) demoted from global"),
        "{log}"
    );
}

#[test]
fn the_report_counts_then_lists() {
    let report = TidyReport {
        changes: vec![
            Change::Created {
                reference: "project/a".into(),
                description: "a".into(),
            },
            Change::Deleted {
                reference: "project/b".into(),
                reason: "stale".into(),
            },
            Change::Created {
                reference: "project/c".into(),
                description: "c".into(),
            },
        ],
    };
    assert_eq!(
        report.render(),
        "tidy: 2 created, 1 deleted\n- created project/a: a\n- deleted project/b: stale\n- created project/c: c"
    );
    assert_eq!(TidyReport::default().render(), super::NOTHING_TO_CHANGE);
}

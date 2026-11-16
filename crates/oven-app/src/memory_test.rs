use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use oven_mem::{
    EMPTY_ID, Evidence, INVALID_ID, MAX_DESCRIPTION, MemoryId, MemoryKind, MemoryRoots,
    MemoryScope, MemoryStore, Note, PROJECT_BUDGET,
};

use super::*;
use crate::core::memory::{DESCRIPTION_LABEL, EXPECTED_SCOPES, TYPE_LABEL, UNKNOWN_SCOPE};

const PROXY_ID: &str = "proxy";
const PROXY_DESCRIPTION: &str = "The internal proxy only speaks HTTP/2.";
const PREF_ID: &str = "clippy";
const PREF_DESCRIPTION: &str = "Run cargo clippy before saying done.";

struct Layers {
    _tmp: tempdir::TempDir,
    root: PathBuf,
}

impl Layers {
    fn new(name: &str) -> Self {
        let tmp = tempdir::TempDir::new(name).unwrap();
        let root = tmp.path().to_path_buf();
        Self { _tmp: tmp, root }
    }

    fn dir(&self, scope: MemoryScope) -> PathBuf {
        self.root.join(scope.as_str())
    }

    fn roots(&self) -> MemoryRoots {
        MemoryRoots {
            workspace: self.root.clone(),
            project: Some(self.dir(MemoryScope::Project)),
            shared: self.dir(MemoryScope::Shared),
            global: Some(self.dir(MemoryScope::Global)),
            inbox: Some(self.root.join("inbox.jsonl")),
        }
    }

    async fn store(&self) -> MemoryStore {
        MemoryStore::load(self.roots()).await
    }

    fn write(&self, scope: MemoryScope, id: &str, front: &str, secs: u64) -> PathBuf {
        let dir = self.dir(scope);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{id}.md"));
        std::fs::write(&path, format!("---\n{front}---\n")).unwrap();
        // SetFileTime needs FILE_WRITE_ATTRIBUTES; a read-only handle is denied on Windows.
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
        path
    }
}

fn fact(description: &str) -> String {
    format!("type: Fact\ndescription: {description}\n")
}

fn preference(description: &str) -> String {
    format!("type: Preference\ndescription: {description}\n")
}

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
    assert_eq!(parse_command("review").unwrap(), MemoryAction::Review);
    assert_eq!(parse_command("tidy").unwrap(), MemoryAction::Tidy);
    assert_eq!(
        parse_command(&format!("show project/{PROXY_ID}")).unwrap(),
        MemoryAction::Show(scoped(MemoryScope::Project, PROXY_ID))
    );
    assert_eq!(
        parse_command(&format!("rm global/{PREF_ID}")).unwrap(),
        MemoryAction::Remove(scoped(MemoryScope::Global, PREF_ID))
    );
    assert_eq!(
        parse_command(&format!("  rm   {PREF_ID}  ")).unwrap(),
        MemoryAction::Remove(MemoryRef::Bare(id(PREF_ID)))
    );
    assert_eq!(
        parse_command(&format!("mv {PREF_ID} global")).unwrap(),
        MemoryAction::Move(MemoryRef::Bare(id(PREF_ID)), MemoryScope::Global)
    );
    assert_eq!(
        parse_command(&format!("approve global/{PREF_ID}")).unwrap(),
        MemoryAction::Approve(scoped(MemoryScope::Global, PREF_ID))
    );
}

#[test]
fn rejects_invalid_forms() {
    for args in [
        "nope",
        "show",
        "rm",
        "show one two",
        "ls",
        "mv x",
        "approve",
    ] {
        let err = parse_command(args).unwrap_err();
        assert_eq!(err.to_string(), USAGE, "{args}");
    }
    let err = parse_command("show user/proxy").unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("{UNKNOWN_SCOPE} 'user'; {EXPECTED_SCOPES}")
    );
    let err = parse_command("mv proxy team").unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("{UNKNOWN_SCOPE} 'team'; {EXPECTED_SCOPES}")
    );
    let err = parse_command("show project/../x").unwrap_err();
    assert_eq!(err.to_string(), INVALID_ID);
    let err = parse_command("rm A").unwrap_err();
    assert_eq!(err.to_string(), INVALID_ID);
    let err = parse_command("show project/").unwrap_err();
    assert_eq!(err.to_string(), EMPTY_ID);
}

#[tokio::test]
async fn list_groups_the_layers_and_says_what_waits() {
    let layers = Layers::new("memory-list");
    assert_eq!(list(&layers.store().await).await, NO_MEMORIES);

    layers.write(MemoryScope::Project, PROXY_ID, &fact(PROXY_DESCRIPTION), 10);
    layers.write(
        MemoryScope::Project,
        "moved",
        &format!("{}anchors: [gone.rs]\n", fact("moved fact")),
        5,
    );
    layers.write(
        MemoryScope::Global,
        PREF_ID,
        &format!(
            "{}verified: {{ by: \"human:me\" }}\n",
            preference(PREF_DESCRIPTION)
        ),
        20,
    );
    layers.write(
        MemoryScope::Global,
        "draft-rule",
        &format!("{}status: draft\n", preference("a draft")),
        30,
    );
    layers.write(
        MemoryScope::Shared,
        "small-prs",
        &preference("small prs"),
        1,
    );
    let store = layers.store().await;
    store
        .inbox()
        .unwrap()
        .append(Note {
            at: "2026-11-16T00:00:00Z".into(),
            session: None,
            kind: MemoryKind::Fact,
            evidence: Evidence::Verified,
            text: "a note".into(),
            about: None,
        })
        .await
        .unwrap();
    assert_eq!(
        list(&store).await,
        format!(
            "\
project:
- {PROXY_ID} (fact) {PROXY_DESCRIPTION}
- moved (fact) moved fact [stale: gone.rs is gone]

shared:
- small-prs (preference) small prs

global:
- draft-rule (preference, draft) a draft
- {PREF_ID} (preference, reviewed) {PREF_DESCRIPTION}

catalog: project 2 of 2, shared 1 of 1, global 1 of 1; 1 draft {AWAIT_REVIEW}; 1 note {WAIT_FOR_TIDY}"
        )
    );
}

#[tokio::test]
async fn a_list_with_only_notes_still_says_they_wait() {
    let layers = Layers::new("memory-list-notes");
    let store = layers.store().await;
    let note = Note {
        at: "2026-11-16T00:00:00Z".into(),
        session: None,
        kind: MemoryKind::Fact,
        evidence: Evidence::Verified,
        text: "a note".into(),
        about: None,
    };
    let inbox = store.inbox().unwrap();
    inbox.append(note.clone()).await.unwrap();
    inbox.append(note).await.unwrap();
    assert_eq!(
        list(&store).await,
        format!("{NO_MEMORIES}\n\n2 notes {WAIT_FOR_TIDY}")
    );
}

#[tokio::test]
async fn list_says_when_a_layer_shows_less_than_it_holds() {
    let layers = Layers::new("memory-list-budget");
    let description = "d".repeat(MAX_DESCRIPTION);
    let count = PROJECT_BUDGET / MAX_DESCRIPTION + 5;
    for n in 0..count {
        layers.write(
            MemoryScope::Project,
            &format!("m{n:03}"),
            &fact(&description),
            10,
        );
    }
    let text = list(&layers.store().await).await;
    let footer = text.rsplit('\n').next().unwrap();
    assert!(footer.starts_with("catalog: project "), "{footer}");
    assert!(
        footer.ends_with(&format!(" of {count} ({BUDGET_FULL})")),
        "{footer}"
    );
}

#[tokio::test]
async fn a_hand_edit_that_breaks_a_memory_is_listed_as_not_loaded() {
    let layers = Layers::new("memory-not-loaded");
    layers.write(MemoryScope::Project, PROXY_ID, &fact(PROXY_DESCRIPTION), 1);
    std::fs::write(
        layers.dir(MemoryScope::Project).join("broken.md"),
        "no fence",
    )
    .unwrap();
    let text = list(&layers.store().await).await;
    assert!(
        text.ends_with("\nnot loaded: project/broken.md (memory file is missing frontmatter)"),
        "{text}"
    );
    let only_broken = Layers::new("memory-not-loaded-only");
    std::fs::create_dir_all(only_broken.dir(MemoryScope::Project)).unwrap();
    std::fs::write(only_broken.dir(MemoryScope::Project).join("broken.md"), "x").unwrap();
    assert!(
        list(&only_broken.store().await)
            .await
            .starts_with(NO_MEMORIES)
    );
}

#[tokio::test]
async fn review_lists_drafts_only() {
    let layers = Layers::new("memory-review");
    assert_eq!(review(&layers.store().await).await, NO_DRAFTS);
    layers.write(MemoryScope::Project, PROXY_ID, &fact(PROXY_DESCRIPTION), 1);
    layers.write(
        MemoryScope::Global,
        "draft-rule",
        &format!("{}status: draft\n", preference("a draft")),
        2,
    );
    assert_eq!(
        review(&layers.store().await).await,
        "global:\n- draft-rule (preference, draft) a draft"
    );
}

#[tokio::test]
async fn show_describes_the_memory() {
    let layers = Layers::new("memory-show");
    layers.write(MemoryScope::Project, PROXY_ID, &fact(PROXY_DESCRIPTION), 1);
    let text = show(&layers.store().await, PROXY_ID).await.unwrap();
    assert_eq!(
        text,
        format!("{TYPE_LABEL}: fact\n{DESCRIPTION_LABEL}: {PROXY_DESCRIPTION}\n")
    );
}

#[tokio::test]
async fn ambiguous_bare_id_names_every_layer() {
    let layers = Layers::new("memory-ambiguous");
    layers.write(MemoryScope::Project, PROXY_ID, &fact("p"), 1);
    layers.write(MemoryScope::Global, PROXY_ID, &fact("g"), 2);
    let text = show(&layers.store().await, PROXY_ID).await.unwrap();
    assert_eq!(
        text,
        format!("{AMBIGUOUS_MEMORY}: project/{PROXY_ID} or global/{PROXY_ID}")
    );
}

#[tokio::test]
async fn remove_unknown_is_a_reply_and_known_is_removed() {
    let layers = Layers::new("memory-rm");
    let path = layers.write(MemoryScope::Project, PROXY_ID, &fact(PROXY_DESCRIPTION), 1);
    let store = layers.store().await;
    assert_eq!(
        remove(&store, "missing").await.unwrap(),
        format!("{NOT_FOUND}: missing")
    );
    assert_eq!(
        remove(&store, &format!("project/{PROXY_ID}"))
            .await
            .unwrap(),
        format!("{REMOVED_MEMORY}: project/{PROXY_ID}")
    );
    assert!(!path.exists());
    assert_eq!(list(&store).await, NO_MEMORIES);
}

#[tokio::test]
async fn move_and_approve_mark_the_memory_reviewed() {
    let layers = Layers::new("memory-mv");
    layers.write(
        MemoryScope::Project,
        PREF_ID,
        &preference(PREF_DESCRIPTION),
        1,
    );
    layers.write(
        MemoryScope::Global,
        "draft-rule",
        &format!("{}status: draft\n", preference("a draft")),
        2,
    );
    let store = layers.store().await;
    assert_eq!(
        move_to(&store, PREF_ID, "global").await.unwrap(),
        format!("{MOVED_MEMORY}: project/{PREF_ID} → global/{PREF_ID}")
    );
    assert_eq!(
        approve(&store, "draft-rule").await.unwrap(),
        format!("{APPROVED_MEMORY}: global/draft-rule")
    );
    let text = list(&store).await;
    assert!(
        text.contains(&format!("- {PREF_ID} (preference, reviewed)")),
        "{text}"
    );
    assert!(
        text.contains("- draft-rule (preference, reviewed) a draft"),
        "{text}"
    );
    assert!(!text.contains(AWAIT_REVIEW), "{text}");
}

#[tokio::test]
async fn edit_path_resolves_a_unique_ref() {
    let layers = Layers::new("memory-edit-path");
    let path = layers.write(MemoryScope::Shared, PROXY_ID, &fact(PROXY_DESCRIPTION), 1);
    let store = layers.store().await;
    assert_eq!(file_path(&store, PROXY_ID).await.unwrap(), path);
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

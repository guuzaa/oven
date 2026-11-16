use std::sync::Arc;

use async_trait::async_trait;
use oven_agent::{Tool, ToolCaps, ToolOutcome, TurnContext};
use oven_mem::{
    Memory, MemoryScope, MemoryStatus, MemoryStore, PutOutcome, SHARED_READ_ONLY, Source, Stamp,
};
use oven_proto::{AgentError, ToolPermission, ToolView};
use serde_json::{Value, json};

use super::view::memory_view;
use super::{kind_property, optional_str, parse_kind, required_str};
use crate::core::memory::{parse_id, parse_scope};
use crate::core::session::SessionStore;

const CREATED_MEMORY: &str = "created memory";
const REPLACED_MEMORY: &str = "replaced memory";
const PREVIOUS_DESCRIPTION: &str = "previous description";
const ANCHORS_NOT_STRINGS: &str = "anchors must be an array of strings";
const REVIEWED_LOCKED: &str = "a person reviewed it, so it cannot be replaced by a model write";
const DRAFT_NOTICE: &str = "it stays a draft until /memory approve, so it is not in the prompt yet";

pub struct MemoryRememberTool {
    store: Arc<MemoryStore>,
    sessions: Option<SessionStore>,
    /// The canonical workspace root, recorded as the memory's project.
    root: String,
}

impl MemoryRememberTool {
    pub const NAME: &'static str = "memory_remember";
    pub const VERB: &'static str = "Remembered";

    pub const DESCRIPTION: &'static str = "\
Save a memory directly. Only for when the user explicitly asks you to
remember something (\"remember that…\", \"from now on…\"); anything you merely
noticed is a memory_note. scope is \"project\" unless the user said it holds
in every project. A global memory is saved as a draft: it is not shown to
you until a person approves it with /memory approve, and the reply says so.
For a preference, description is the rule itself and why is its reason; a
preference has no body. For a fact, description is one line, body holds the
command or path that makes it usable, and anchors lists the workspace-relative
files it is about. Reuse an existing id to revise a memory; a memory a person
reviewed cannot be replaced this way. Never save a secret.";

    pub fn new(store: Arc<MemoryStore>, sessions: Option<SessionStore>, root: String) -> Self {
        Self {
            store,
            sessions,
            root,
        }
    }
}

#[async_trait]
impl Tool for MemoryRememberTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &'static str {
        Self::DESCRIPTION
    }

    fn view(&self, input: &Value) -> ToolView {
        memory_view(Self::VERB, input)
    }

    fn schema(&self) -> Arc<Value> {
        Arc::new(json!({
            "type": "object",
            "properties": {
                "scope": { "type": "string", "enum": ["project", "global"] },
                "id": { "type": "string", "description": "Slug to create or replace, e.g. \"proxy-requires-http2\"." },
                "kind": kind_property(),
                "description": { "type": "string", "description": "One catalog line; for a preference, the rule." },
                "why": { "type": "string", "description": "One sentence: the reason." },
                "body": { "type": "string", "description": "Facts only: the command or path that makes it usable." },
                "anchors": { "type": "array", "items": { "type": "string" }, "description": "Facts only: workspace-relative paths it is about." }
            },
            "required": ["scope", "id", "kind", "description"]
        }))
    }

    fn caps(&self) -> ToolCaps {
        ToolCaps {
            permission: ToolPermission::Write,
            ..Default::default()
        }
    }

    async fn run(&self, args: &Value, cx: &TurnContext) -> Result<ToolOutcome, AgentError> {
        let tool_error = |message: String| AgentError::from(format!("{}: {message}", Self::NAME));
        let scope = parse_scope(required_str(args, "scope", Self::NAME)?).map_err(tool_error)?;
        if scope == MemoryScope::Shared {
            return Err(tool_error(SHARED_READ_ONLY.to_owned()));
        }
        let id = parse_id(required_str(args, "id", Self::NAME)?).map_err(AgentError::from)?;
        let kind = parse_kind(args, Self::NAME)?;
        let description = required_str(args, "description", Self::NAME)?;
        let mut memory = Memory::new(id, scope, kind, description);
        memory.why = optional_str(args, "why").map(str::to_owned);
        memory.body = args
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        memory.anchors = anchors(args).ok_or_else(|| tool_error(ANCHORS_NOT_STRINGS.to_owned()))?;
        memory.generated = Some(Stamp::model(cx.model().0.as_str()));
        memory.add_source(Source::project(&self.root));
        if let Some(sessions) = &self.sessions {
            memory.add_source(Source::session(&sessions.current_id()));
        }
        let drafted = scope == MemoryScope::Global;
        if drafted {
            memory.status = MemoryStatus::Draft;
        }
        if let Ok(existing) = self.store.read(scope, &memory.id).await
            && existing.is_reviewed()
        {
            return Err(tool_error(REVIEWED_LOCKED.to_owned()));
        }
        let reference = memory.reference();
        let outcome = self
            .store
            .put(memory)
            .await
            .map_err(|err| AgentError::from(err.to_string()))?;
        Ok(outcome_text(&reference, outcome, drafted).into())
    }
}

/// The `anchors` argument; `None` when it is not an array of strings.
fn anchors(args: &Value) -> Option<Vec<String>> {
    match args.get("anchors") {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect(),
        Some(_) => None,
    }
}

fn outcome_text(reference: &str, outcome: PutOutcome, drafted: bool) -> String {
    let base = match outcome {
        PutOutcome::Created => format!("{CREATED_MEMORY} {reference}"),
        PutOutcome::Replaced {
            previous_description,
        } => format!(
            "{REPLACED_MEMORY} {reference}; {PREVIOUS_DESCRIPTION}: \"{previous_description}\""
        ),
    };
    match drafted {
        true => format!("{base}; {DRAFT_NOTICE}"),
        false => base,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use oven_agent::{CancellationToken, Selection, Tool, ToolOutcome, TurnContext};
    use oven_llm::ModelId;
    use oven_mem::{
        MemoryId, MemoryRoots, MemoryScope, MemoryStatus, MemoryStore, PREFERENCE_BODY,
        SHARED_READ_ONLY, Source,
    };
    use oven_proto::{AgentMode, SESSION_FORMAT_VERSION, TurnId};
    use serde_json::json;

    use super::{
        ANCHORS_NOT_STRINGS, CREATED_MEMORY, DRAFT_NOTICE, MemoryRememberTool,
        PREVIOUS_DESCRIPTION, REPLACED_MEMORY, REVIEWED_LOCKED,
    };
    use crate::core::session::{Session, SessionStore};

    const SESSION: &str = "01J8Z";
    const MODEL: &str = "m1";
    const ROOT: &str = "/ws";

    fn turn() -> TurnContext {
        TurnContext::new(
            TurnId::next(),
            CancellationToken::new(),
            Selection::new(AgentMode::Agent, ModelId::new(MODEL), None),
        )
    }

    fn roots(tmp: &tempdir::TempDir) -> MemoryRoots {
        MemoryRoots {
            workspace: tmp.path().to_path_buf(),
            project: Some(tmp.path().join("project")),
            shared: tmp.path().join("shared"),
            global: Some(tmp.path().join("global")),
            inbox: None,
        }
    }

    async fn tool(tmp: &tempdir::TempDir, session: bool) -> MemoryRememberTool {
        let store = MemoryStore::load(roots(tmp)).await;
        let sessions = match session {
            true => {
                let session = Session::open(&tmp.path().join("sessions"), SESSION)
                    .await
                    .unwrap();
                Some(SessionStore::new(
                    session,
                    tmp.path(),
                    true,
                    SESSION_FORMAT_VERSION,
                ))
            }
            false => None,
        };
        MemoryRememberTool::new(Arc::new(store), sessions, ROOT.into())
    }

    fn args(description: &str) -> serde_json::Value {
        json!({
            "scope": "project",
            "id": "proxy",
            "kind": "fact",
            "description": description,
            "body": "use http2\n",
            "anchors": ["Cargo.toml"],
        })
    }

    #[tokio::test]
    async fn records_who_wrote_it_and_where_it_came_from() {
        let tmp = tempdir::TempDir::new("memory-remember").unwrap();
        let tool = tool(&tmp, true).await;
        let ToolOutcome { output, .. } = tool.run(&args("proxy fact"), &turn()).await.unwrap();
        assert_eq!(output, format!("{CREATED_MEMORY} project/proxy"));
        let store = MemoryStore::load(roots(&tmp)).await;
        let memory = store
            .read(MemoryScope::Project, &MemoryId::new("proxy").unwrap())
            .await
            .unwrap();
        assert_eq!(
            memory.generated.as_ref().unwrap().by,
            format!("oven/{MODEL}")
        );
        assert_eq!(
            memory.sources,
            vec![Source::project(ROOT), Source::session(SESSION)]
        );
        assert_eq!(memory.anchors, vec!["Cargo.toml"]);
        assert!(!memory.is_reviewed());
    }

    #[tokio::test]
    async fn reports_replaced_with_the_previous_description() {
        let tmp = tempdir::TempDir::new("memory-remember-replace").unwrap();
        let tool = tool(&tmp, false).await;
        tool.run(&args("first fact"), &turn()).await.unwrap();
        let ToolOutcome { output, .. } = tool.run(&args("revised fact"), &turn()).await.unwrap();
        assert_eq!(
            output,
            format!("{REPLACED_MEMORY} project/proxy; {PREVIOUS_DESCRIPTION}: \"first fact\"")
        );
    }

    #[tokio::test]
    async fn a_global_memory_is_a_draft_until_a_person_approves_it() {
        let tmp = tempdir::TempDir::new("memory-remember-global").unwrap();
        let tool = tool(&tmp, false).await;
        let mut global = args("everywhere");
        global["scope"] = json!("global");
        global["id"] = json!("everywhere");
        global["anchors"] = json!([]);
        let ToolOutcome { output, .. } = tool.run(&global, &turn()).await.unwrap();
        assert_eq!(
            output,
            format!("{CREATED_MEMORY} global/everywhere; {DRAFT_NOTICE}")
        );
        let store = MemoryStore::load(roots(&tmp)).await;
        let memory = store
            .read(MemoryScope::Global, &MemoryId::new("everywhere").unwrap())
            .await
            .unwrap();
        assert_eq!(memory.status, MemoryStatus::Draft);
        assert!(
            !store
                .catalog()
                .is_some_and(|c| c.text.contains("everywhere"))
        );
    }

    #[tokio::test]
    async fn a_memory_a_person_reviewed_cannot_be_replaced() {
        let tmp = tempdir::TempDir::new("memory-remember-reviewed").unwrap();
        let global = tmp.path().join("global");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(
            global.join("rule.md"),
            "---\ntype: Preference\ndescription: approved rule\nverified: { by: \"human:me\" }\n---\n",
        )
        .unwrap();
        let tool = tool(&tmp, false).await;
        let mut replace = args("model rewrite");
        replace["scope"] = json!("global");
        replace["id"] = json!("rule");
        replace["kind"] = json!("preference");
        replace["body"] = json!("");
        replace["anchors"] = json!([]);
        let err = tool.run(&replace, &turn()).await.unwrap_err();
        assert_eq!(err.message, format!("memory_remember: {REVIEWED_LOCKED}"));
        let raw = std::fs::read_to_string(global.join("rule.md")).unwrap();
        assert!(raw.contains("approved rule"));
    }

    #[tokio::test]
    async fn refuses_shared_a_preference_body_and_bad_anchors() {
        let tmp = tempdir::TempDir::new("memory-remember-refuse").unwrap();
        let tool = tool(&tmp, false).await;
        let mut shared = args("x");
        shared["scope"] = json!("shared");
        let err = tool.run(&shared, &turn()).await.unwrap_err();
        assert_eq!(err.message, format!("memory_remember: {SHARED_READ_ONLY}"));

        let mut preference = args("rule");
        preference["kind"] = json!("preference");
        let err = tool.run(&preference, &turn()).await.unwrap_err();
        assert_eq!(err.message, PREFERENCE_BODY);

        let mut anchors = args("x");
        anchors["anchors"] = json!("Cargo.toml");
        let err = tool.run(&anchors, &turn()).await.unwrap_err();
        assert_eq!(
            err.message,
            format!("memory_remember: {ANCHORS_NOT_STRINGS}")
        );
        assert!(!tmp.path().join("project").exists());
    }
}

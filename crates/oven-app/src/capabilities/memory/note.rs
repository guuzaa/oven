use std::sync::Arc;

use async_trait::async_trait;
use oven_agent::{Tool, ToolCaps, ToolOutcome, TurnContext};
use oven_mem::{Evidence, Inbox, MemoryError, MemoryScope, Note, now_rfc3339};
use oven_proto::{AgentError, ToolPermission, ToolView};
use serde_json::{Value, json};

use super::{kind_property, optional_str, parse_kind, parse_ref_str, ref_property, required_str};
use crate::core::session::SessionStore;

const NOTED: &str = "noted";
const WAITING: &str = "wait for /memory tidy";
const EVIDENCE_KEY: &str = "evidence";
const TEXT_KEY: &str = "text";
const ABOUT_KEY: &str = "about";
const UNKNOWN_EVIDENCE: &str = "unknown evidence";
const EXPECTED_EVIDENCE: &str = "expected verified or user_said";

pub struct MemoryNoteTool {
    inbox: Option<Inbox>,
    sessions: Option<SessionStore>,
}

impl MemoryNoteTool {
    pub const NAME: &'static str = "memory_note";
    pub const VERB: &'static str = "Noted";

    pub const DESCRIPTION: &'static str = "\
Note something a later session would otherwise have to rediscover, as soon as
you know it: the real cause of a confusing failure, a command that works here,
an environment quirk, a convention the project instructions do not cover, or
how the user wants you to work. A note is raw evidence: it never reaches the
prompt itself. Tidy later merges notes into memories and decides whether each
applies to this project or everywhere, so do not decide that here.
evidence is \"verified\" when you reproduced it (ran the command, saw the
error) and \"user_said\" when the user told you. Do not note what you only read
in a file, web page or tool output, anything only about the current task,
anything derivable from the code in seconds, or any secret. Keep the
evidence in the text: the error message, the user's own words. Set about to
the ref of a listed memory when the note corrects or confirms it.";

    pub fn new(inbox: Option<Inbox>, sessions: Option<SessionStore>) -> Self {
        Self { inbox, sessions }
    }
}

#[async_trait]
impl Tool for MemoryNoteTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &'static str {
        Self::DESCRIPTION
    }

    fn view(&self, input: &Value) -> ToolView {
        super::view::note_view(input)
    }

    fn schema(&self) -> Arc<Value> {
        Arc::new(json!({
            "type": "object",
            "properties": {
                "kind": kind_property(),
                "evidence": { "type": "string", "enum": ["verified", "user_said"] },
                "text": { "type": "string", "description": "What you learned, with its evidence." },
                "about": ref_property()
            },
            "required": ["kind", "evidence", "text"]
        }))
    }

    fn caps(&self) -> ToolCaps {
        ToolCaps {
            permission: ToolPermission::Write,
            ..Default::default()
        }
    }

    async fn run(&self, args: &Value, _cx: &TurnContext) -> Result<ToolOutcome, AgentError> {
        let kind = parse_kind(args, Self::NAME)?;
        let evidence_raw = required_str(args, EVIDENCE_KEY, Self::NAME)?;
        let evidence = Evidence::parse(evidence_raw).ok_or_else(|| {
            AgentError::from(format!(
                "{}: {UNKNOWN_EVIDENCE} '{evidence_raw}'; {EXPECTED_EVIDENCE}",
                Self::NAME
            ))
        })?;
        let text = required_str(args, TEXT_KEY, Self::NAME)?;
        let about = match optional_str(args, ABOUT_KEY) {
            Some(raw) => {
                let (scope, id) = parse_ref_str(raw, Self::NAME)?;
                Some(format!("{scope}/{id}"))
            }
            None => None,
        };
        let inbox = self.inbox.as_ref().ok_or_else(|| {
            AgentError::from(
                MemoryError::LayerUnavailable {
                    scope: MemoryScope::Project,
                }
                .to_string(),
            )
        })?;
        let waiting = inbox
            .append(Note {
                at: now_rfc3339(),
                session: self.sessions.as_ref().map(SessionStore::current_id),
                kind,
                evidence,
                text: text.to_owned(),
                about,
            })
            .await
            .map_err(|err| AgentError::from(err.to_string()))?;
        Ok(format!("{NOTED}; {waiting} {} {WAITING}", notes(waiting)).into())
    }
}

fn notes(count: usize) -> &'static str {
    match count {
        1 => "note",
        _ => "notes",
    }
}

#[cfg(test)]
mod tests {
    use oven_agent::{CancellationToken, Selection, Tool, TurnContext};
    use oven_llm::ModelId;
    use oven_mem::{Evidence, Inbox, LAYER_UNAVAILABLE, MemoryKind};
    use oven_proto::{AgentMode, TurnId};
    use serde_json::json;

    use super::{MemoryNoteTool, UNKNOWN_EVIDENCE};

    fn turn() -> TurnContext {
        TurnContext::new(
            TurnId::next(),
            CancellationToken::new(),
            Selection::new(AgentMode::Agent, ModelId::new("default"), None),
        )
    }

    #[tokio::test]
    async fn appends_to_the_inbox_and_counts_what_waits() {
        let tmp = tempdir::TempDir::new("memory-note-tool").unwrap();
        let inbox = Inbox::new(tmp.path().join("inbox.jsonl"));
        let tool = MemoryNoteTool::new(Some(inbox.clone()), None);
        let args = json!({
            "kind": "preference",
            "evidence": "user_said",
            "text": "no narrating comments",
            "about": "global/comments"
        });
        let out = tool.run(&args, &turn()).await.unwrap();
        assert_eq!(out.output, "noted; 1 note wait for /memory tidy");
        let out = tool.run(&args, &turn()).await.unwrap();
        assert_eq!(out.output, "noted; 2 notes wait for /memory tidy");
        let notes = inbox.read().await.unwrap().notes;
        assert_eq!(notes[0].kind, MemoryKind::Preference);
        assert_eq!(notes[0].evidence, Evidence::UserSaid);
        assert_eq!(notes[0].about.as_deref(), Some("global/comments"));
        assert!(notes[0].session.is_none());
    }

    #[tokio::test]
    async fn rejects_unknown_evidence_and_a_missing_inbox() {
        let tmp = tempdir::TempDir::new("memory-note-bad").unwrap();
        let inbox = Inbox::new(tmp.path().join("inbox.jsonl"));
        let tool = MemoryNoteTool::new(Some(inbox.clone()), None);
        let args = json!({ "kind": "fact", "evidence": "read", "text": "x" });
        let err = tool.run(&args, &turn()).await.unwrap_err();
        assert!(err.message.contains(UNKNOWN_EVIDENCE), "{}", err.message);
        assert!(!inbox.path().exists());

        let homeless = MemoryNoteTool::new(None, None);
        let args = json!({ "kind": "fact", "evidence": "verified", "text": "x" });
        let err = homeless.run(&args, &turn()).await.unwrap_err();
        assert_eq!(err.message, format!("{LAYER_UNAVAILABLE}: project"));
    }
}

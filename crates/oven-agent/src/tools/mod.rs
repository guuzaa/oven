mod answer;
mod bash;
mod catalog;
mod file_edit;
mod file_read;
mod file_write;
mod glob;
mod grep;
mod skill_read;
mod todo_write;
mod view;

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::question::{
    AnswerResponse, Question, QuestionRequest, QuestionRequestId, QuestionSender,
};

pub const NO_USER_TO_ANSWER: &str = "no user is available to answer the question";

pub use answer::AnswerTool;
pub use bash::BashTool;
pub use catalog::{BUILTIN_TOOLS, BuiltinTool};
pub use file_edit::FileEditTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use skill_read::SkillReadTool;
pub use todo_write::TodoWriteTool;
pub(crate) use view::labeled;
pub use view::{ToolCaps, ToolPermission, ToolView, present_tool};

/// What a tool may do besides its own arguments: observe the turn's
/// cancellation and, for interactive tools, ask the user a question.
pub struct ToolContext<'a> {
    cancel: Option<&'a CancellationToken>,
    asker: Option<&'a QuestionSender>,
}

impl<'a> ToolContext<'a> {
    /// Both halves are optional: `new(None, None)` is the shape a tool sees
    /// when it is invoked outside a frontend-driven turn.
    pub const fn new(
        cancel: Option<&'a CancellationToken>,
        asker: Option<&'a QuestionSender>,
    ) -> Self {
        Self { cancel, asker }
    }

    pub fn cancel(&self) -> Option<&'a CancellationToken> {
        self.cancel
    }

    /// Puts `question` to the user and waits for the reply, giving up as
    /// cancelled when the turn is cancelled or the frontend goes away.
    pub(crate) async fn ask(&self, question: Question) -> Result<AnswerResponse, AgentError> {
        let asker = self
            .asker
            .ok_or_else(|| AgentError::from(NO_USER_TO_ANSWER))?;
        let (responder, reply) = oneshot::channel();
        asker
            .send(QuestionRequest {
                request_id: QuestionRequestId::next(),
                question,
                responder,
            })
            .map_err(|_| AgentError::from(NO_USER_TO_ANSWER))?;
        let reply = match self.cancel {
            Some(cancel) => tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(AgentError::cancelled()),
                reply = reply => reply,
            },
            None => reply.await,
        };
        reply.map_err(|_| AgentError::cancelled())
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> Value;
    fn view(&self, _input: &Value) -> ToolView {
        ToolView::named(self.name())
    }
    fn caps(&self) -> ToolCaps {
        ToolCaps::default()
    }
    async fn run(&self, args: &Value, ctx: &ToolContext<'_>) -> Result<String, AgentError>;
}

pub(crate) fn resolve_within(root: &Path, rel: &str) -> Result<PathBuf, AgentError> {
    oven_host::resolve_within(root, rel).map_err(|error| AgentError::from(error.to_string()))
}

pub(crate) fn require_str<'a>(
    args: &'a Value,
    key: &str,
    tool: &str,
) -> Result<&'a str, AgentError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| AgentError::from(format!("{tool}: missing '{key}' string argument")))
}

pub(crate) fn parse_limit(args: &Value, default: usize) -> usize {
    args.get("limit")
        .and_then(Value::as_i64)
        .map(|v| v.max(0))
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(default)
}

//! Tools the agent's own tests mount in place of the real ones.
//!
//! The driver promises that a call reaches the history, that its detail
//! lands in the transcript and that a permission gates it. A test needs a
//! name, a permission and an output to promise that with — not a file
//! system, and not a tool whose behaviour belongs to another layer.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::core::tool::Tool;
use crate::core::turn::TurnContext;
use crate::core::view::{ToolCaps, ToolEffect, ToolOutcome};
use oven_proto::AgentError;
use oven_proto::{AnswerResponse, Question, QuestionOption, TodoList, ToolPermission};

/// A tool that answers with a fixed output, under a fixed permission.
pub(crate) struct StubTool {
    name: &'static str,
    permission: ToolPermission,
    plan_only: bool,
    output: String,
    detail: Option<String>,
    /// A file the call creates, so a test can tell whether it ran.
    creates: Option<PathBuf>,
}

impl StubTool {
    pub(crate) fn new(name: &'static str, output: &str) -> Self {
        Self {
            name,
            permission: ToolPermission::Read,
            plan_only: false,
            output: output.to_string(),
            detail: None,
            creates: None,
        }
    }

    pub(crate) fn with_permission(mut self, permission: ToolPermission) -> Self {
        self.permission = permission;
        self
    }

    pub(crate) fn with_detail(mut self, detail: &str) -> Self {
        self.detail = Some(detail.to_string());
        self
    }

    /// A file the call creates, so a test can tell whether it ran.
    pub(crate) fn creating(mut self, path: impl Into<PathBuf>) -> Self {
        self.creates = Some(path.into());
        self
    }
}

#[async_trait]
impl Tool for StubTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "a tool that answers with a fixed output"
    }

    fn schema(&self) -> Arc<Value> {
        Arc::new(json!({ "type": "object" }))
    }

    fn caps(&self) -> ToolCaps {
        ToolCaps {
            plan_only: self.plan_only,
            permission: self.permission,
            exclusive: false,
        }
    }

    async fn run(&self, _args: &Value, _cx: &TurnContext) -> Result<ToolOutcome, AgentError> {
        if let Some(path) = &self.creates {
            std::fs::write(path, "written").unwrap();
        }
        Ok(ToolOutcome {
            output: self.output.clone(),
            detail: self.detail.clone(),
        })
    }
}

/// A tool that asks the user a question, the way a tool that needs an answer
/// does.
pub(crate) struct AskTool {
    question: String,
    options: Vec<QuestionOption>,
}

impl AskTool {
    pub(crate) fn new(question: &str, options: Vec<QuestionOption>) -> Self {
        Self {
            question: question.to_string(),
            options,
        }
    }
}

#[async_trait]
impl Tool for AskTool {
    fn name(&self) -> &str {
        "answer"
    }

    fn description(&self) -> &str {
        "a tool that asks the user a question"
    }

    fn schema(&self) -> Arc<Value> {
        Arc::new(json!({ "type": "object" }))
    }

    fn caps(&self) -> ToolCaps {
        ToolCaps {
            exclusive: true,
            ..Default::default()
        }
    }

    async fn run(&self, _args: &Value, cx: &TurnContext) -> Result<ToolOutcome, AgentError> {
        let answer = cx
            .ask(Question {
                question: self.question.clone(),
                options: self.options.clone(),
            })
            .await?;
        match answer {
            AnswerResponse::Answered { answer } => Ok(ToolOutcome {
                output: format!("the user answered: {answer}"),
                detail: Some(answer),
            }),
            AnswerResponse::Declined => Ok("the user skipped the question".into()),
        }
    }
}

/// A tool that replaces the checklist, the way a tool that owns the plan
/// does.
pub(crate) struct TodoTool;

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str {
        "todo_write"
    }

    fn description(&self) -> &str {
        "a tool that replaces the checklist"
    }

    fn schema(&self) -> Arc<Value> {
        Arc::new(json!({ "type": "object" }))
    }

    fn caps(&self) -> ToolCaps {
        ToolCaps {
            plan_only: true,
            permission: ToolPermission::Write,
            exclusive: true,
        }
    }

    fn effect(&self, input: &Value) -> Option<ToolEffect> {
        TodoList::parse(input).ok().map(ToolEffect::Todos)
    }

    async fn run(&self, args: &Value, _cx: &TurnContext) -> Result<ToolOutcome, AgentError> {
        Ok(TodoList::parse(args)
            .map_err(AgentError::from)?
            .summary()
            .into())
    }
}

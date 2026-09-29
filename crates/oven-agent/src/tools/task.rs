//! Delegation tools: `task` starts a subagent, `task_output` reports on the
//! ones already started.
//!
//! The tools own the wire format the model sees and the prose it reads back;
//! whoever implements [`SubagentSpawner`] owns everything else.

use std::fmt::Write;
use std::sync::Arc;

use async_trait::async_trait;
use oven_llm::Usage;
use serde_json::{Value, json};

use crate::error::AgentError;
use crate::mode::AgentMode;
use crate::subagent::{
    NodeInfo, NodeOutcome, NodeReport, NodeStatus, RoleSpec, SpawnRequest, SubagentSpawner,
};
use crate::tools::{Tool, ToolCaps, ToolPermission, ToolView, require_str};
use crate::turn::TurnContext;

const BACKGROUND_HINT: &str = "Read its result later with task_output.";
const NO_SUBAGENTS: &str = "no subagents have run in this session";
const PLAN_MODE_NOTE: &str = " In plan mode only the read-only roles may run.";

/// A subagent's report is a tool result like any other, so it is capped
/// before it reaches the caller's context.
const MAX_REPORT_BYTES: usize = 16 * 1024;
const TRUNCATION_MARK: &str = "\n...[truncated]";

pub struct TaskTool {
    spawner: Arc<dyn SubagentSpawner>,
    roles: Vec<RoleSpec>,
    description: String,
}

impl TaskTool {
    pub const NAME: &str = "task";

    pub fn new(spawner: Arc<dyn SubagentSpawner>, roles: Vec<RoleSpec>) -> Self {
        let description = describe(&roles);
        Self {
            spawner,
            roles,
            description,
        }
    }

    fn role(&self, name: &str) -> Result<&RoleSpec, AgentError> {
        self.roles
            .iter()
            .find(|role| role.name == name)
            .ok_or_else(|| {
                AgentError::from(format!(
                    "{}: unknown role '{}'; available: {}",
                    Self::NAME,
                    name,
                    self.role_names()
                ))
            })
    }

    fn role_names(&self) -> String {
        self.roles
            .iter()
            .map(|role| role.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn parse(&self, args: &Value, cx: &TurnContext) -> Result<SpawnRequest, AgentError> {
        let label = require_str(args, "description", Self::NAME)?.to_string();
        let prompt = require_str(args, "prompt", Self::NAME)?.to_string();
        let role = match args.get("role").and_then(Value::as_str) {
            Some(name) => self.role(name)?,
            None => self.roles.first().ok_or_else(|| {
                AgentError::from(format!("{}: no roles are available", Self::NAME))
            })?,
        };
        if cx.mode() == AgentMode::Plan && !role.read_only {
            return Err(AgentError::from(format!(
                "{}: role '{}' writes; plan mode allows only {}",
                Self::NAME,
                role.name,
                self.read_only_names()
            )));
        }
        let (model, reasoning_effort) = cx.model();
        Ok(SpawnRequest {
            role: role.name.clone(),
            label,
            prompt,
            background: args
                .get("background")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            model,
            reasoning_effort,
            parent_turn: cx.turn_id,
            cancellation: cx.cancellation.clone(),
        })
    }

    fn read_only_names(&self) -> String {
        let names: Vec<&str> = self
            .roles
            .iter()
            .filter(|role| role.read_only)
            .map(|role| role.name.as_str())
            .collect();
        match names.is_empty() {
            true => "no roles; use your own tools".to_string(),
            false => names.join(", "),
        }
    }

    pub fn view_input(input: &Value) -> ToolView {
        let label = input.get("description").and_then(Value::as_str);
        let role = input.get("role").and_then(Value::as_str);
        let summary = match (role, label) {
            (Some(role), Some(label)) => format!("Agent {role}: {label}"),
            (None, Some(label)) => format!("Agent: {label}"),
            _ => Self::NAME.to_string(),
        };
        ToolView {
            summary,
            collapse: true,
            detail: None,
        }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn caps(&self) -> ToolCaps {
        ToolCaps {
            permission: ToolPermission::External,
            ..Default::default()
        }
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "Short label for this subagent, shown to the user. Not a prompt."
                },
                "prompt": {
                    "type": "string",
                    "description": "The task, in full: the subagent does not see this conversation, and only its final answer comes back."
                },
                "role": {
                    "type": "string",
                    "enum": self.roles.iter().map(|role| role.name.clone()).collect::<Vec<_>>(),
                    "description": "Which subagent to run. Defaults to the first listed role."
                },
                "background": {
                    "type": "boolean",
                    "description": "Start it and return immediately instead of waiting for the result."
                }
            },
            "required": ["description", "prompt"]
        })
    }

    fn view(&self, input: &Value) -> ToolView {
        Self::view_input(input)
    }

    async fn run(&self, args: &Value, cx: &TurnContext) -> Result<String, AgentError> {
        let request = self.parse(args, cx)?;
        let background = request.background;
        let handle = self.spawner.spawn(request).await?;
        match background {
            true => Ok(format!(
                "[{} started in the background. {BACKGROUND_HINT}]",
                handle.name
            )),
            false => {
                let name = handle.name.clone();
                Ok(render_outcome(&name, handle.join().await?))
            }
        }
    }
}

pub struct TaskOutputTool {
    spawner: Arc<dyn SubagentSpawner>,
}

impl TaskOutputTool {
    pub const NAME: &str = "task_output";

    pub fn new(spawner: Arc<dyn SubagentSpawner>) -> Self {
        Self { spawner }
    }

    pub fn view_input(input: &Value) -> ToolView {
        let summary = match input.get("name").and_then(Value::as_str) {
            Some(name) => format!("Agent status: {name}"),
            None => "Agent status".to_string(),
        };
        ToolView {
            summary,
            collapse: true,
            detail: None,
        }
    }
}

#[async_trait]
impl Tool for TaskOutputTool {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Report on subagents started with `task`: their status, what they have spent, and the result of a finished one. Omit `name` to list every subagent in this session."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Subagent to report on, e.g. `explore#1`. Omit to list all of them."
                }
            }
        })
    }

    fn view(&self, input: &Value) -> ToolView {
        Self::view_input(input)
    }

    async fn run(&self, args: &Value, _cx: &TurnContext) -> Result<String, AgentError> {
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty());
        // The spawner describes what went wrong with the name as it was
        // asked; the tool names itself, so the model reads one sentence.
        let reports = self
            .spawner
            .reports(name)
            .map_err(|e| AgentError::from(format!("{}: {}", Self::NAME, e.message)))?;
        if reports.is_empty() {
            return Ok(NO_SUBAGENTS.to_string());
        }
        Ok(render_reports(&reports))
    }
}

fn describe(roles: &[RoleSpec]) -> String {
    let mut text = String::from(
        "Delegate a self-contained task to a subagent with its own context window. \
         The subagent cannot see this conversation, so `prompt` must stand alone, and \
         only its final answer comes back, so ask for a concise result. Subagents run \
         concurrently with you, cannot ask the user anything, and cannot spawn their \
         own subagents. Roles:",
    );
    for role in roles {
        let _ = write!(text, " `{}` — {};", role.name, role.description);
    }
    text.push_str(PLAN_MODE_NOTE);
    text
}

fn render_outcome(name: &str, outcome: NodeOutcome) -> String {
    let mut text = format!(
        "[{name} {} · {} · {} · {}]",
        outcome.status.label(),
        tally(outcome.steps, outcome.tool_calls),
        tokens(outcome.usage),
        seconds(outcome.duration_ms)
    );
    match &outcome.status {
        NodeStatus::Failed { error } => {
            let _ = write!(text, "\n{error}");
        }
        _ => {
            let report = outcome.report.trim();
            if !report.is_empty() {
                let _ = write!(text, "\n\n{}", clamp(report, MAX_REPORT_BYTES));
            }
        }
    }
    text
}

fn render_reports(reports: &[NodeReport]) -> String {
    let mut out = String::new();
    for report in reports {
        if !out.is_empty() {
            out.push('\n');
        }
        let _ = write!(
            out,
            "{} · {} · {} · {} · {}",
            report.info.name,
            report.info.status.label(),
            tally(report.info.steps, report.info.tool_calls),
            tokens(report.info.usage),
            elapsed(&report.info)
        );
        match &report.report {
            Some(report) if !report.trim().is_empty() => {
                let _ = write!(out, "\n  {}", clamp(report.trim(), MAX_REPORT_BYTES));
            }
            _ => {}
        }
    }
    out
}

fn tally(steps: u32, tool_calls: u32) -> String {
    match steps {
        0 => "not started".to_string(),
        _ => format!("{steps} steps, {tool_calls} tool calls"),
    }
}

fn tokens(usage: Usage) -> String {
    let total = usage
        .input_tokens
        .saturating_add(usage.output_tokens)
        .saturating_add(usage.cache_read_tokens);
    format!("{total} tokens")
}

fn seconds(duration_ms: u64) -> String {
    format!("{:.1}s", duration_ms as f64 / 1000.0)
}

fn elapsed(info: &NodeInfo) -> String {
    seconds(info.elapsed_ms())
}

fn clamp(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    format!(
        "{}{TRUNCATION_MARK}",
        &text[..text.floor_char_boundary(max)]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{AgentId, TurnId};
    use crate::subagent::NodeHandle;
    use crate::turn::TurnContext;
    use serde_json::json;
    use std::sync::Mutex;
    use tokio::sync::oneshot;

    const REPORT: &str = "found it in runtime/mod.rs";

    fn role(name: &str, read_only: bool) -> RoleSpec {
        RoleSpec {
            name: name.to_string(),
            description: format!("{name} things"),
            read_only,
        }
    }

    fn usage(input: u32, output: u32) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
        }
    }

    fn info(name: &str, status: NodeStatus) -> NodeInfo {
        NodeInfo {
            id: AgentId::next(),
            name: name.to_string(),
            role: "explore".into(),
            label: "find it".into(),
            background: false,
            parent: AgentId::next(),
            status,
            usage: usage(1000, 40),
            tool_calls: 3,
            steps: 4,
            started_at: oven_host::now_ms(),
            finished_at: None,
        }
    }

    struct MockSpawner {
        seen: Mutex<Vec<SpawnRequest>>,
        outcome: NodeOutcome,
        reports: Vec<NodeReport>,
        /// Returned instead of `reports` when set, to exercise the tool's own
        /// error wording.
        reports_error: Option<String>,
    }

    impl MockSpawner {
        fn new(outcome: NodeOutcome) -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(Vec::new()),
                outcome,
                reports: Vec::new(),
                reports_error: None,
            })
        }

        fn with_reports(outcome: NodeOutcome, reports: Vec<NodeReport>) -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(Vec::new()),
                outcome,
                reports,
                reports_error: None,
            })
        }

        fn with_reports_error(message: &str) -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(Vec::new()),
                outcome: completed(),
                reports: Vec::new(),
                reports_error: Some(message.to_string()),
            })
        }
    }

    #[async_trait]
    impl SubagentSpawner for MockSpawner {
        async fn spawn(&self, request: SpawnRequest) -> Result<NodeHandle, AgentError> {
            self.seen.lock().unwrap().push(request);
            let (tx, rx) = oneshot::channel();
            tx.send(self.outcome.clone()).unwrap();
            Ok(NodeHandle::new(AgentId::next(), "explore#1".into(), rx))
        }

        fn reports(&self, _name: Option<&str>) -> Result<Vec<NodeReport>, AgentError> {
            match &self.reports_error {
                Some(message) => Err(AgentError::from(message.clone())),
                None => Ok(self.reports.clone()),
            }
        }
    }

    fn tool(spawner: Arc<MockSpawner>) -> TaskTool {
        TaskTool::new(spawner, vec![role("explore", true), role("general", false)])
    }

    fn completed() -> NodeOutcome {
        NodeOutcome {
            status: NodeStatus::Completed,
            report: REPORT.to_string(),
            tool_calls: 6,
            steps: 3,
            usage: usage(8000, 400),
            duration_ms: 32_400,
        }
    }

    #[tokio::test]
    async fn foreground_returns_the_report_with_a_tally() {
        let spawner = MockSpawner::new(completed());
        let out = tool(spawner.clone())
            .run(
                &json!({"description": "find it", "prompt": "go"}),
                &TurnContext::for_test(),
            )
            .await
            .unwrap();
        assert!(out.contains("explore#1"), "{out}");
        assert!(out.contains("3 steps, 6 tool calls"), "{out}");
        assert!(out.contains("8400 tokens"), "{out}");
        assert!(out.contains("32.4s"), "{out}");
        assert!(out.ends_with(REPORT), "{out}");

        let seen = spawner.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].role, "explore", "the first role is the default");
        assert!(!seen[0].background);
        assert_eq!(seen[0].label, "find it");
    }

    #[tokio::test]
    async fn background_returns_without_waiting() {
        let spawner = MockSpawner::new(completed());
        let out = tool(spawner.clone())
            .run(
                &json!({"description": "find it", "prompt": "go", "background": true}),
                &TurnContext::for_test(),
            )
            .await
            .unwrap();
        assert!(out.contains("explore#1 started in the background"), "{out}");
        assert!(out.contains("task_output"), "{out}");
        assert!(spawner.seen.lock().unwrap()[0].background);
    }

    #[tokio::test]
    async fn an_unknown_role_lists_the_available_ones() {
        let err = tool(MockSpawner::new(completed()))
            .run(
                &json!({"description": "x", "prompt": "y", "role": "nope"}),
                &TurnContext::for_test(),
            )
            .await
            .unwrap_err();
        assert!(
            err.message.contains("unknown role 'nope'"),
            "{}",
            err.message
        );
        assert!(err.message.contains("explore, general"), "{}", err.message);
    }

    #[tokio::test]
    async fn plan_mode_refuses_a_writing_role() {
        let cx = TurnContext::for_test_in(AgentMode::Plan);
        let err = tool(MockSpawner::new(completed()))
            .run(
                &json!({"description": "x", "prompt": "y", "role": "general"}),
                &cx,
            )
            .await
            .unwrap_err();
        assert!(
            err.message.contains("plan mode allows only explore"),
            "{}",
            err.message
        );

        tool(MockSpawner::new(completed()))
            .run(
                &json!({"description": "x", "prompt": "y", "role": "explore"}),
                &cx,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_failed_subagent_reports_its_error_instead_of_a_report() {
        let outcome = NodeOutcome {
            status: NodeStatus::Failed {
                error: "provider exploded".into(),
            },
            ..completed()
        };
        let out = tool(MockSpawner::new(outcome))
            .run(
                &json!({"description": "find it", "prompt": "go"}),
                &TurnContext::for_test(),
            )
            .await
            .unwrap();
        assert!(out.contains("failed"), "{out}");
        assert!(out.contains("provider exploded"), "{out}");
        assert!(!out.contains(REPORT), "{out}");
    }

    #[tokio::test]
    async fn a_long_report_is_clamped() {
        let outcome = NodeOutcome {
            report: "x".repeat(MAX_REPORT_BYTES * 2),
            ..completed()
        };
        let out = tool(MockSpawner::new(outcome))
            .run(
                &json!({"description": "find it", "prompt": "go"}),
                &TurnContext::for_test(),
            )
            .await
            .unwrap();
        assert!(out.ends_with(TRUNCATION_MARK), "{}", out.len());
        assert!(out.len() < MAX_REPORT_BYTES + 200, "{}", out.len());
    }

    #[tokio::test]
    async fn missing_arguments_are_refused() {
        let err = tool(MockSpawner::new(completed()))
            .run(&json!({"description": "x"}), &TurnContext::for_test())
            .await
            .unwrap_err();
        assert!(err.message.contains("missing 'prompt'"), "{}", err.message);
    }

    #[tokio::test]
    async fn task_output_lists_subagents() {
        let reports = vec![NodeReport {
            info: info(
                "explore#1",
                NodeStatus::Running {
                    turn_id: TurnId::next(),
                },
            ),
            report: None,
        }];
        let out = TaskOutputTool::new(MockSpawner::with_reports(completed(), reports))
            .run(&json!({}), &TurnContext::for_test())
            .await
            .unwrap();
        assert!(out.contains("explore#1 · running"), "{out}");
        assert!(out.contains("4 steps, 3 tool calls"), "{out}");
    }

    #[tokio::test]
    async fn task_output_says_so_when_nothing_ran() {
        let out = TaskOutputTool::new(MockSpawner::new(completed()))
            .run(&json!({"name": "explore#1"}), &TurnContext::for_test())
            .await
            .unwrap();
        assert_eq!(out, NO_SUBAGENTS);
    }

    #[tokio::test]
    async fn task_output_names_itself_in_an_error() {
        let err = TaskOutputTool::new(MockSpawner::with_reports_error("unknown subagent 'nope'"))
            .run(&json!({"name": "nope"}), &TurnContext::for_test())
            .await
            .unwrap_err();
        assert_eq!(err.message, format!("task_output: unknown subagent 'nope'"));
    }

    #[test]
    fn description_lists_every_role() {
        let text = describe(&[role("explore", true), role("general", false)]);
        assert!(text.contains("`explore` — explore things"), "{text}");
        assert!(text.contains("`general` — general things"), "{text}");
        assert!(text.contains(PLAN_MODE_NOTE), "{text}");
    }

    #[test]
    fn view_names_the_role_and_label() {
        let view = TaskTool::view_input(&json!({"description": "find it", "role": "explore"}));
        assert_eq!(view.summary, "Agent explore: find it");
        assert_eq!(TaskTool::view_input(&json!({})).summary, TaskTool::NAME);
    }
}

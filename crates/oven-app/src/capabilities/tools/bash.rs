use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{labeled, require_str};
use oven_agent::{Tool, ToolCaps, ToolOutcome, TurnContext};
use oven_host::{CommandError, run_shell_command};
use oven_proto::AgentError;
use oven_proto::{ToolPermission, ToolView};

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const MAX_TIMEOUT_SECS: u64 = 86_400;

pub struct BashTool {
    root: PathBuf,
    timeout: Duration,
}

impl BashTool {
    pub const NAME: &'static str = "bash";
    pub const VERB: &'static str = "Ran";

    pub fn view_input(input: &Value) -> ToolView {
        labeled(Self::VERB, input, "command")
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn resolve_timeout(&self, args: &Value) -> Result<Duration, AgentError> {
        let Some(value) = args.get("timeout_secs") else {
            return Ok(self.timeout);
        };
        let secs = value.as_u64().ok_or_else(|| {
            AgentError::from("bash: 'timeout_secs' must be a positive integer number of seconds")
        })?;
        if !(1..=MAX_TIMEOUT_SECS).contains(&secs) {
            return Err(AgentError::from(format!(
                "bash: 'timeout_secs' must be between 1 and {MAX_TIMEOUT_SECS}, got {secs}"
            )));
        }
        Ok(Duration::from_secs(secs))
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn view(&self, input: &Value) -> ToolView {
        Self::view_input(input)
    }
    fn caps(&self) -> ToolCaps {
        ToolCaps {
            permission: ToolPermission::Execute,
            ..Default::default()
        }
    }
    fn description(&self) -> &'static str {
        "Execute a command with the host shell in the workspace root and return stdout/stderr. Uses PowerShell on Windows and bash (falling back to sh) elsewhere. Use for builds, tests, git, etc. Set timeout_secs for commands that need longer than the default 300 seconds."
    }
    fn schema(&self) -> Arc<Value> {
        Arc::new(json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command to execute with the host shell." },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_TIMEOUT_SECS,
                    "description": "Seconds to wait before the command is killed. Defaults to 300."
                }
            },
            "required": ["command"]
        }))
    }
    async fn run(&self, args: &Value, cx: &TurnContext) -> Result<ToolOutcome, AgentError> {
        let command = require_str(args, "command", Self::NAME)?;
        let timeout = self.resolve_timeout(args)?;
        let output = run_shell_command(command, &self.root, timeout, Some(&cx.cancellation))
            .await
            .map_err(|error| match error {
                CommandError::Cancelled { .. } => AgentError::cancelled(),
                CommandError::Spawn(error) => AgentError::from(format!("bash: spawn: {error}")),
                CommandError::Wait(error) => AgentError::from(format!("bash: wait: {error}")),
                error @ CommandError::TimedOut { .. } => AgentError::from(format!("bash: {error}")),
            })?;

        let mut text = String::new();
        if !output.stdout.is_empty() {
            text.push_str(&output.stdout);
        }
        if !output.stderr.is_empty() {
            if text.is_empty() {
                text.push_str("--- stderr ---\n");
            } else {
                text.push_str("\n--- stderr ---\n");
            }
            text.push_str(&output.stderr);
        }
        if let Some(status) = output.status
            && !status.success()
        {
            let _ = write!(text, "\n[exit code: {}]", status.code().unwrap_or(-1));
        }
        if text.is_empty() {
            text.push_str("(no output)");
        }
        Ok(text.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    use crate::capabilities::tools::test_context;

    fn tmp_dir() -> tempdir::TempDir {
        tempdir::TempDir::new("oven-test").unwrap()
    }

    fn sleep_command(seconds: u64) -> String {
        #[cfg(windows)]
        {
            format!("Start-Sleep -Seconds {seconds}")
        }
        #[cfg(not(windows))]
        {
            format!("sleep {seconds}")
        }
    }

    fn read_marker_command() -> &'static str {
        #[cfg(windows)]
        {
            "Get-Content -Raw marker.txt"
        }
        #[cfg(not(windows))]
        {
            "cat marker.txt"
        }
    }

    #[tokio::test]
    async fn bash_runs_command_and_captures_output() {
        let tmp = tmp_dir();
        let bash = BashTool::new(tmp.path());
        let ToolOutcome { output: out, .. } = bash
            .run(&json!({"command": "echo hi"}), &test_context())
            .await
            .unwrap();
        assert!(out.contains("hi"), "{out}");
    }

    #[tokio::test]
    async fn bash_reports_nonzero_exit() {
        let tmp = tmp_dir();
        let bash = BashTool::new(tmp.path());
        let ToolOutcome { output: out, .. } = bash
            .run(&json!({"command": "exit 7"}), &test_context())
            .await
            .unwrap();
        assert!(out.contains("[exit code: 7]"), "{out}");
    }

    #[tokio::test]
    async fn bash_times_out() {
        let tmp = tmp_dir();
        let bash = BashTool::new(tmp.path()).with_timeout(Duration::from_millis(100));
        let err = bash
            .run(&json!({"command": sleep_command(5)}), &test_context())
            .await
            .unwrap_err();
        assert!(err.message.contains("timed out"), "{}", err.message);
    }

    #[tokio::test]
    async fn bash_timeout_secs_argument_overrides_default() {
        let tmp = tmp_dir();
        let bash = BashTool::new(tmp.path());
        let err = bash
            .run(
                &json!({"command": sleep_command(5), "timeout_secs": 1}),
                &test_context(),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("timed out"), "{}", err.message);
    }

    #[test]
    fn bash_timeout_defaults_to_300_seconds() {
        let tmp = tmp_dir();
        let bash = BashTool::new(tmp.path());
        assert_eq!(
            bash.resolve_timeout(&json!({})).unwrap(),
            Duration::from_secs(300)
        );
        assert_eq!(
            bash.resolve_timeout(&json!({"timeout_secs": 42})).unwrap(),
            Duration::from_secs(42)
        );
    }

    #[tokio::test]
    async fn bash_rejects_invalid_timeout_secs() {
        let tmp = tmp_dir();
        let bash = BashTool::new(tmp.path());
        for value in [json!(0), json!(-1), json!("60"), json!(100_000)] {
            let err = bash
                .run(
                    &json!({"command": "echo hi", "timeout_secs": value}),
                    &test_context(),
                )
                .await
                .unwrap_err();
            assert!(
                err.message.contains("timeout_secs"),
                "unexpected error for {value}: {}",
                err.message
            );
        }
    }

    #[tokio::test]
    async fn bash_runs_in_workspace_root() {
        let tmp = tmp_dir();
        let root: PathBuf = tmp.path().to_path_buf();
        std::fs::write(root.join("marker.txt"), "found").unwrap();
        let bash = BashTool::new(&root);
        let ToolOutcome { output: out, .. } = bash
            .run(&json!({"command": read_marker_command()}), &test_context())
            .await
            .unwrap();
        assert_eq!(out.trim(), "found");
    }

    #[tokio::test]
    async fn bash_cancel_aborts_and_returns_cancelled() {
        let tmp = tmp_dir();
        let bash = BashTool::new(tmp.path());
        let turn = test_context();
        let cancel = turn.cancellation.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();

        let handle = tokio::spawn(async move {
            let result = bash
                .run(&json!({"command": sleep_command(60)}), &turn)
                .await;
            let _ = tx.send(result);
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        let result = tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .expect("cancel should resolve promptly")
            .expect("tool task alive");
        let err = result.expect_err("expected cancellation error");
        assert!(err.is_cancelled());
        handle.await.unwrap();
    }
}

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolView, labeled, require_str};
use crate::error::AgentError;
use oven_host::{CommandError, run_shell_command};

pub struct BashTool {
    root: PathBuf,
    timeout: Duration,
}

impl BashTool {
    pub const NAME: &'static str = "bash";

    pub fn view_input(input: &Value) -> ToolView {
        labeled(Self::NAME, "Ran", input, "command")
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            timeout: Duration::from_secs(300),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
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
    fn description(&self) -> &str {
        "Execute a command with the host shell in the workspace root and return stdout/stderr. Uses PowerShell on Windows and bash (falling back to sh) elsewhere. Use for builds, tests, git, etc."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command to execute with the host shell." }
            },
            "required": ["command"]
        })
    }
    async fn run(
        &self,
        args: &Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError> {
        let command = require_str(args, "command", Self::NAME)?;
        let output = run_shell_command(command, &self.root, self.timeout, cancel)
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
            if !text.is_empty() {
                text.push_str("\n--- stderr ---\n");
            } else {
                text.push_str("--- stderr ---\n");
            }
            text.push_str(&output.stderr);
        }
        if let Some(status) = output.status
            && !status.success()
        {
            text.push_str(&format!("\n[exit code: {}]", status.code().unwrap_or(-1)));
        }
        if text.is_empty() {
            text.push_str("(no output)");
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

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
        let out = bash
            .run(&json!({"command": "echo hi"}), None)
            .await
            .unwrap();
        assert!(out.contains("hi"), "{out}");
    }

    #[tokio::test]
    async fn bash_reports_nonzero_exit() {
        let tmp = tmp_dir();
        let bash = BashTool::new(tmp.path());
        let out = bash.run(&json!({"command": "exit 7"}), None).await.unwrap();
        assert!(out.contains("[exit code: 7]"), "{out}");
    }

    #[tokio::test]
    async fn bash_times_out() {
        let tmp = tmp_dir();
        let bash = BashTool::new(tmp.path()).with_timeout(Duration::from_millis(100));
        let err = bash
            .run(&json!({"command": sleep_command(5)}), None)
            .await
            .unwrap_err();
        assert!(err.message.contains("timed out"), "{}", err.message);
    }

    #[tokio::test]
    async fn bash_runs_in_workspace_root() {
        let tmp = tmp_dir();
        let root: PathBuf = tmp.path().to_path_buf();
        std::fs::write(root.join("marker.txt"), "found").unwrap();
        let bash = BashTool::new(&root);
        let out = bash
            .run(&json!({"command": read_marker_command()}), None)
            .await
            .unwrap();
        assert_eq!(out.trim(), "found");
    }

    #[tokio::test]
    async fn bash_cancel_aborts_and_returns_cancelled() {
        let tmp = tmp_dir();
        let bash = BashTool::new(tmp.path());
        let cancel = CancellationToken::new();
        let (tx, rx) = tokio::sync::oneshot::channel();

        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            let result = bash
                .run(
                    &json!({"command": sleep_command(60)}),
                    Some(&cancel_for_task),
                )
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

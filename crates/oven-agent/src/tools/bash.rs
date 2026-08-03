use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::{Tool, require_str};
use crate::error::AgentError;

pub struct BashTool {
    root: PathBuf,
    timeout: Duration,
    max_output: usize,
}

impl BashTool {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            timeout: Duration::from_secs(30),
            max_output: 4000,
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
        "bash"
    }
    fn description(&self) -> &str {
        "Execute a shell command in the workspace root and return stdout/stderr. Use for running builds, tests, git, etc."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to execute." }
            },
            "required": ["command"]
        })
    }
    async fn run(
        &self,
        args: &Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, AgentError> {
        let command = require_str(args, "command", "bash")?;
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command).current_dir(&self.root);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let child = cmd
            .spawn()
            .map_err(|e| AgentError::from(format!("bash: spawn: {}", e)))?;

        // The wait future owns the child; dropping it (cancel or timeout)
        // kills the process because `kill_on_drop` is set.
        let mut wait = Box::pin(tokio::time::timeout(self.timeout, child.wait_with_output()));
        let output = if let Some(c) = cancel {
            let picked = tokio::select! {
                biased;
                _ = c.cancelled() => None,
                res = wait.as_mut() => Some(res),
            };
            match picked {
                None => return Err(AgentError::cancelled()),
                Some(res) => res,
            }
        } else {
            wait.await
        };

        let output = match output {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Err(AgentError::from(format!("bash: wait: {}", e))),
            Err(_) => {
                return Err(AgentError::from(format!(
                    "bash: command timed out after {}s",
                    self.timeout.as_secs()
                )));
            }
        };

        let mut text = String::new();
        if !output.stdout.is_empty() {
            text.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        if !output.stderr.is_empty() {
            if !text.is_empty() {
                text.push_str("\n--- stderr ---\n");
            } else {
                text.push_str("--- stderr ---\n");
            }
            text.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        if !output.status.success() {
            text.push_str(&format!(
                "\n[exit code: {}]",
                output.status.code().unwrap_or(-1)
            ));
        }
        if text.len() > self.max_output {
            let end = text.floor_char_boundary(self.max_output);
            text.truncate(end);
            text.push_str("\n...[output truncated]");
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
            .run(&json!({"command": "sleep 5"}), None)
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
            .run(&json!({"command": "cat marker.txt"}), None)
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
                    &json!({"command": "sleep 60; echo done"}),
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

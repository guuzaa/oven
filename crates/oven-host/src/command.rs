use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use thiserror::Error;

use crate::decode::decode_command_output;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: Option<ExitStatus>,
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("failed to spawn command: {0}")]
    Spawn(std::io::Error),
    #[error("failed to wait for command: {0}")]
    Wait(std::io::Error),
    #[error("command timed out after {}s", timeout.as_secs())]
    TimedOut {
        timeout: Duration,
        output: CommandOutput,
    },
    #[error("command cancelled")]
    Cancelled { output: CommandOutput },
}

pub async fn run_shell_command(
    command: &str,
    current_dir: &Path,
    timeout: Duration,
    cancel: Option<&CancellationToken>,
) -> Result<CommandOutput, CommandError> {
    let mut child = spawn_host_shell(current_dir, command).map_err(CommandError::Spawn)?;
    let stdout = tokio::spawn(read_pipe(child.stdout.take()));
    let stderr = tokio::spawn(read_pipe(child.stderr.take()));
    let result = {
        let wait = tokio::time::timeout(timeout, child.wait());
        tokio::pin!(wait);
        if let Some(cancel) = cancel {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => None,
                result = &mut wait => Some(result),
            }
        } else {
            Some(wait.await)
        }
    };

    match result {
        Some(Ok(Ok(status))) => Ok(join_output(stdout, stderr, Some(status)).await),
        Some(Ok(Err(error))) => Err(CommandError::Wait(error)),
        Some(Err(_)) => {
            drop(child);
            Err(CommandError::TimedOut {
                timeout,
                output: join_output(stdout, stderr, None).await,
            })
        }
        None => {
            drop(child);
            Err(CommandError::Cancelled {
                output: join_output(stdout, stderr, None).await,
            })
        }
    }
}

fn spawn_host_shell(current_dir: &Path, command: &str) -> std::io::Result<Child> {
    #[cfg(windows)]
    {
        spawn_with(
            current_dir,
            "powershell.exe",
            &[
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                command,
            ],
        )
    }
    #[cfg(not(windows))]
    {
        match spawn_with(current_dir, "bash", &["-c", command]) {
            Ok(child) => Ok(child),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                spawn_with(current_dir, "sh", &["-c", command])
            }
            Err(error) => Err(error),
        }
    }
}

fn spawn_with(current_dir: &Path, program: &str, args: &[&str]) -> std::io::Result<Child> {
    let mut process = Command::new(program);
    process
        .args(args)
        .current_dir(current_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    process.creation_flags(CREATE_NO_WINDOW);
    process.spawn()
}

async fn read_pipe<R: tokio::io::AsyncRead + Unpin + Send + 'static>(pipe: Option<R>) -> Vec<u8> {
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };
    let mut buffer = Vec::new();
    let _ = pipe.read_to_end(&mut buffer).await;
    buffer
}

async fn join_output(
    stdout: tokio::task::JoinHandle<Vec<u8>>,
    stderr: tokio::task::JoinHandle<Vec<u8>>,
    status: Option<ExitStatus>,
) -> CommandOutput {
    let stdout = stdout.await.unwrap_or_default();
    let stderr = stderr.await.unwrap_or_default();
    CommandOutput {
        stdout: decode_command_output(&stdout),
        stderr: decode_command_output(&stderr),
        status,
    }
}

use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, OwnedHandle, RawHandle};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
};

use crate::decode::decode_command_output;

#[cfg(windows)]
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const READ_CHUNK: usize = 8 * 1024;
/// Pipes only report EOF once every writer closes them, and a detached
/// descendant (daemon, `nohup`, `setsid`) may never do so. Once the command
/// itself has exited, keep reading for this long, then stop and keep whatever
/// arrived.
const OUTPUT_GRACE: Duration = Duration::from_secs(1);

type OutputBuffer = Arc<Mutex<Vec<u8>>>;

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
    let tree = ProcessTree::attach(&child);
    let captured = Captured::start(&mut child);

    let waited = {
        let wait = tokio::time::timeout(timeout, child.wait());
        tokio::pin!(wait);
        if let Some(cancel) = cancel {
            tokio::select! {
                biased;
                () = cancel.cancelled() => None,
                result = &mut wait => Some(result),
            }
        } else {
            Some(wait.await)
        }
    };

    let Some(waited) = waited else {
        tree.kill();
        drop(child);
        let output = captured.finish(None).await;
        return Err(CommandError::Cancelled { output });
    };

    match waited {
        Ok(Ok(status)) => Ok(captured.finish(Some(status)).await),
        Ok(Err(error)) => {
            tree.kill();
            drop(child);
            Err(CommandError::Wait(error))
        }
        Err(_) => {
            tree.kill();
            drop(child);
            let output = captured.finish(None).await;
            Err(CommandError::TimedOut { timeout, output })
        }
    }
}

/// Every process the shell spawned, so that a timeout stops the work itself
/// instead of only its shell.
struct ProcessTree {
    #[cfg(unix)]
    pgid: Option<libc::pid_t>,
    #[cfg(windows)]
    job: Option<OwnedHandle>,
}

#[cfg(unix)]
impl ProcessTree {
    fn attach(child: &Child) -> Self {
        Self {
            pgid: child.id().map(|pid| pid as libc::pid_t),
        }
    }

    fn kill(&self) {
        let Some(pgid) = self.pgid else {
            return;
        };
        // SAFETY: `pgid` is the group created for the child by `process_group(0)`,
        // and a negative pid signals every member of that group.
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
    }
}

#[cfg(windows)]
impl ProcessTree {
    fn attach(child: &Child) -> Self {
        Self {
            job: child.raw_handle().and_then(create_job),
        }
    }

    fn kill(&self) {
        let Some(job) = &self.job else {
            return;
        };
        const KILLED_EXIT_CODE: u32 = 1;
        // SAFETY: `job` is a live handle to the job that owns the child process.
        unsafe { TerminateJobObject(job.as_raw_handle(), KILLED_EXIT_CODE) };
    }
}

#[cfg(windows)]
fn create_job(process: RawHandle) -> Option<OwnedHandle> {
    // SAFETY: null attributes and name request an anonymous, non-inheritable job.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return None;
    }
    // SAFETY: `job` is a fresh non-null handle that this function now owns.
    let job = unsafe { OwnedHandle::from_raw_handle(job) };
    // SAFETY: both handles are valid and owned here, and joining the job is what
    // lets one termination call reach every process the child spawns.
    let joined = unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) };
    (joined != 0).then_some(job)
}

struct Captured {
    stdout: OutputBuffer,
    stderr: OutputBuffer,
    stdout_reader: JoinHandle<()>,
    stderr_reader: JoinHandle<()>,
}

impl Captured {
    fn start(child: &mut Child) -> Self {
        let stdout = OutputBuffer::default();
        let stderr = OutputBuffer::default();
        Self {
            stdout_reader: read_pipe(child.stdout.take(), Arc::clone(&stdout)),
            stderr_reader: read_pipe(child.stderr.take(), Arc::clone(&stderr)),
            stdout,
            stderr,
        }
    }

    async fn finish(mut self, status: Option<ExitStatus>) -> CommandOutput {
        let _ = tokio::time::timeout(OUTPUT_GRACE, async {
            let _ = tokio::join!(&mut self.stdout_reader, &mut self.stderr_reader);
        })
        .await;
        self.stdout_reader.abort();
        self.stderr_reader.abort();
        CommandOutput {
            stdout: take(&self.stdout),
            stderr: take(&self.stderr),
            status,
        }
    }
}

impl Drop for Captured {
    fn drop(&mut self) {
        self.stdout_reader.abort();
        self.stderr_reader.abort();
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
    #[cfg(unix)]
    process.process_group(0);
    #[cfg(windows)]
    process.creation_flags(CREATE_NO_WINDOW);
    process.spawn()
}

fn read_pipe<R>(pipe: Option<R>, buffer: OutputBuffer) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let Some(mut pipe) = pipe else {
            return;
        };
        let mut chunk = [0; READ_CHUNK];
        while let Ok(read) = pipe.read(&mut chunk).await {
            if read == 0 {
                break;
            }
            buffer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(&chunk[..read]);
        }
    })
}

fn take(buffer: &Mutex<Vec<u8>>) -> String {
    let bytes = std::mem::take(&mut *buffer.lock().unwrap_or_else(PoisonError::into_inner));
    decode_command_output(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const BUDGET: Duration = Duration::from_secs(5);
    const HEARTBEAT_FILE: &str = "heartbeat.txt";

    /// A background loop that keeps writing to `HEARTBEAT_FILE` while the
    /// foreground command sleeps.
    #[cfg(unix)]
    fn heartbeat_command() -> String {
        format!("(while :; do printf . >> {HEARTBEAT_FILE}; sleep 0.05; done) & sleep 30")
    }
    #[cfg(windows)]
    fn heartbeat_command() -> String {
        format!(
            r#"cmd /c "for /l %i in (1,0,1) do @(echo x>>{HEARTBEAT_FILE} & ping -n 2 127.0.0.1 >nul)""#
        )
    }

    #[cfg(unix)]
    const TREE_TIMEOUT: Duration = Duration::from_secs(1);
    #[cfg(windows)]
    const TREE_TIMEOUT: Duration = Duration::from_secs(3);

    /// Longer than the heartbeat interval, so a surviving descendant is
    /// guaranteed to write again.
    #[cfg(unix)]
    const SETTLE: Duration = Duration::from_millis(500);
    #[cfg(windows)]
    const SETTLE: Duration = Duration::from_millis(2500);

    #[tokio::test]
    async fn timeout_stops_the_spawned_tree() {
        let dir = tempdir::TempDir::new("oven-host-tree").unwrap();
        let error = run_shell_command(&heartbeat_command(), dir.path(), TREE_TIMEOUT, None)
            .await
            .unwrap_err();
        assert!(matches!(error, CommandError::TimedOut { .. }), "{error}");
        let heartbeat = dir.path().join(HEARTBEAT_FILE);
        let written = file_size(&heartbeat);
        assert!(written > 0, "the descendant never started writing");
        tokio::time::sleep(SETTLE).await;
        assert_eq!(
            file_size(&heartbeat),
            written,
            "the descendant survived the timeout"
        );
    }

    fn file_size(path: &Path) -> u64 {
        std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detached_descendant_does_not_block_completion() {
        let started = Instant::now();
        let output = run_shell_command(
            "echo started; sleep 30 & echo $!",
            Path::new(env!("CARGO_MANIFEST_DIR")),
            BUDGET,
            None,
        )
        .await
        .unwrap();
        assert!(output.stdout.contains("started"), "{:?}", output.stdout);
        assert!(started.elapsed() < BUDGET, "{:?}", started.elapsed());
        let pid: libc::pid_t = output
            .stdout
            .lines()
            .last()
            .and_then(|line| line.trim().parse().ok())
            .expect("background pid");
        // SAFETY: the pid was printed by the shell above; killing it only ends
        // the job this test started.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

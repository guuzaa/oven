use std::fmt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use oven_agent::{CancellationToken, decode_command_output};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub(crate) const HOST_SHELL_TIMEOUT: Duration = Duration::from_secs(300);
const HISTORY_MAX_LINES: usize = 200;
const HISTORY_MAX_BYTES: usize = 32 * 1024;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const STDERR_MARK: &str = "--- stderr ---";
const NO_OUTPUT: &str = "(no output)";
const BANG: char = '!';
const ENVELOPE_OPEN: &str = "<local-shell>";
const ENVELOPE_CLOSE: &str = "</local-shell>";
const COMMAND_OPEN: &str = "<command>";
const COMMAND_CLOSE: &str = "</command>";
const EXIT_OPEN: &str = "<exit_code>";
const EXIT_CLOSE: &str = "</exit_code>";
const ERROR_OPEN: &str = "<error>";
const ERROR_CLOSE: &str = "</error>";
const OUTPUT_OPEN: &str = "<output>";
const OUTPUT_CLOSE: &str = "</output>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalShell {
    pub command: String,
    pub exit_code: Option<i32>,
    pub output: String,
    pub error: Option<String>,
}

impl LocalShell {
    pub fn ok(&self) -> bool {
        self.error.is_none() && self.exit_code.unwrap_or(0) == 0
    }

    pub fn try_parse(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        if !trimmed.starts_with(ENVELOPE_OPEN) || !trimmed.ends_with(ENVELOPE_CLOSE) {
            return None;
        }
        let command = xml_unescape(&tagged(trimmed, COMMAND_OPEN, COMMAND_CLOSE)?);
        if command.is_empty() {
            return None;
        }
        let exit_code = tagged(trimmed, EXIT_OPEN, EXIT_CLOSE).and_then(|s| s.parse().ok());
        let error = tagged(trimmed, ERROR_OPEN, ERROR_CLOSE).map(|s| xml_unescape(&s));
        let output = tagged(trimmed, OUTPUT_OPEN, OUTPUT_CLOSE)
            .map(|s| xml_unescape(s.trim()))
            .unwrap_or_default();
        Some(Self {
            command,
            exit_code,
            output,
            error,
        })
    }
}

impl fmt::Display for LocalShell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{ENVELOPE_OPEN}")?;
        writeln!(
            f,
            "{COMMAND_OPEN}{}{COMMAND_CLOSE}",
            xml_escape(&self.command)
        )?;
        if let Some(code) = self.exit_code {
            writeln!(f, "{EXIT_OPEN}{code}{EXIT_CLOSE}")?;
        }
        if let Some(error) = &self.error {
            writeln!(f, "{ERROR_OPEN}{}{ERROR_CLOSE}", xml_escape(error))?;
        }
        writeln!(f, "{OUTPUT_OPEN}")?;
        writeln!(f, "{}", xml_escape(&cap_output(&self.output)))?;
        writeln!(f, "{OUTPUT_CLOSE}")?;
        write!(f, "{ENVELOPE_CLOSE}")
    }
}

/// Composer / `StartTurn` text that begins with `!`. Persisted history uses
/// [`parse_local_shell`] on the `<local-shell>` envelope instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellInput<'a> {
    command: &'a str,
}

impl<'a> ShellInput<'a> {
    pub fn parse(input: &'a str) -> Option<Self> {
        let rest = input.trim_start().strip_prefix(BANG)?;
        Some(Self {
            command: rest.trim(),
        })
    }

    pub fn command(&self) -> Option<&'a str> {
        if self.command.is_empty() {
            None
        } else {
            Some(self.command)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellOutput {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShellError {
    Spawn(String),
    Wait(String),
    Timeout { secs: u64, output: ShellOutput },
    Cancelled { output: ShellOutput },
}

pub fn display_shell_line(command: &str) -> String {
    format!("{BANG} {command}")
}

pub(crate) fn commit_shell(command: &str, result: Result<ShellOutput, ShellError>) -> LocalShell {
    match result {
        Ok(output) => {
            let text = format_shell_text(&output);
            LocalShell {
                command: command.to_string(),
                exit_code: output.exit_code,
                output: text,
                error: None,
            }
        }
        Err(ShellError::Cancelled { output }) => LocalShell {
            command: command.to_string(),
            exit_code: None,
            output: format_shell_text(&output),
            error: Some("cancelled".into()),
        },
        Err(ShellError::Timeout { secs, output }) => {
            let err = format!("command timed out after {secs}s");
            let mut text = format_shell_text(&output);
            if text == NO_OUTPUT {
                text = err.clone();
            } else if !text.ends_with('\n') {
                text.push('\n');
                text.push_str(&err);
            } else {
                text.push_str(&err);
            }
            LocalShell {
                command: command.to_string(),
                exit_code: None,
                output: text,
                error: Some(err),
            }
        }
        Err(ShellError::Spawn(e) | ShellError::Wait(e)) => LocalShell {
            command: command.to_string(),
            exit_code: None,
            output: e.clone(),
            error: Some(e),
        },
    }
}

fn format_shell_text(output: &ShellOutput) -> String {
    let mut text = String::new();
    if !output.stdout.is_empty() {
        text.push_str(&output.stdout);
    }
    if !output.stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(STDERR_MARK);
        text.push('\n');
        text.push_str(&output.stderr);
    }
    if let Some(code) = output.exit_code
        && code != 0
    {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("[exit code: {code}]"));
    }
    if text.is_empty() {
        NO_OUTPUT.to_string()
    } else {
        text
    }
}

enum Waited {
    Status(std::process::ExitStatus),
    Wait(std::io::Error),
    Timeout,
    Cancelled,
}

pub(crate) async fn run_host_shell(
    root: &Path,
    command: &str,
    timeout: Duration,
    cancel: Option<&CancellationToken>,
) -> Result<ShellOutput, ShellError> {
    let mut child =
        spawn_host(root, command).map_err(|e| ShellError::Spawn(format!("shell: spawn: {e}")))?;
    let stdout_task = tokio::spawn(read_pipe(child.stdout.take()));
    let stderr_task = tokio::spawn(read_pipe(child.stderr.take()));

    let waited = if let Some(token) = cancel {
        let mut wait = std::pin::pin!(tokio::time::timeout(timeout, child.wait()));
        tokio::select! {
            biased;
            _ = token.cancelled() => Waited::Cancelled,
            res = &mut wait => match res {
                Ok(Ok(status)) => Waited::Status(status),
                Ok(Err(e)) => Waited::Wait(e),
                Err(_) => Waited::Timeout,
            },
        }
    } else {
        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) => Waited::Status(status),
            Ok(Err(e)) => Waited::Wait(e),
            Err(_) => Waited::Timeout,
        }
    };

    match waited {
        Waited::Status(status) => Ok(join_output(stdout_task, stderr_task, status.code()).await),
        Waited::Wait(e) => Err(ShellError::Wait(format!("shell: wait: {e}"))),
        Waited::Cancelled => {
            drop(child);
            Err(ShellError::Cancelled {
                output: join_output(stdout_task, stderr_task, None).await,
            })
        }
        Waited::Timeout => {
            drop(child);
            Err(ShellError::Timeout {
                secs: timeout.as_secs(),
                output: join_output(stdout_task, stderr_task, None).await,
            })
        }
    }
}

fn spawn_host(root: &Path, command: &str) -> std::io::Result<tokio::process::Child> {
    #[cfg(windows)]
    {
        spawn_with(
            root,
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
        match spawn_with(root, "bash", &["-c", command]) {
            Ok(child) => Ok(child),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                spawn_with(root, "sh", &["-c", command])
            }
            Err(e) => Err(e),
        }
    }
}

fn spawn_with(root: &Path, program: &str, args: &[&str]) -> std::io::Result<tokio::process::Child> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()
}

async fn read_pipe<R: tokio::io::AsyncRead + Unpin + Send + 'static>(pipe: Option<R>) -> Vec<u8> {
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    let _ = pipe.read_to_end(&mut buf).await;
    buf
}

async fn join_output(
    stdout: tokio::task::JoinHandle<Vec<u8>>,
    stderr: tokio::task::JoinHandle<Vec<u8>>,
    exit_code: Option<i32>,
) -> ShellOutput {
    let stdout = stdout.await.unwrap_or_default();
    let stderr = stderr.await.unwrap_or_default();
    ShellOutput {
        stdout: decode_command_output(&stdout),
        stderr: decode_command_output(&stderr),
        exit_code,
    }
}

fn cap_output(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut body = if lines.len() > HISTORY_MAX_LINES {
        let skip = lines.len() - HISTORY_MAX_LINES;
        format!(
            "… truncated {skip} earlier lines\n{}",
            lines[skip..].join("\n")
        )
    } else {
        output.to_string()
    };
    if body.len() > HISTORY_MAX_BYTES {
        let extra = body.len() - HISTORY_MAX_BYTES;
        let start = body.floor_char_boundary(body.len().saturating_sub(HISTORY_MAX_BYTES));
        body = format!("… truncated {extra} earlier bytes\n{}", &body[start..]);
    }
    body
}

fn tagged(text: &str, open: &str, close: &str) -> Option<String> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(text[start..end].to_string())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(windows)]
    const NIHAO: &str = "你好";
    #[cfg(windows)]
    const REPLACEMENT: char = '\u{FFFD}';
    #[cfg(windows)]
    const GBK_NIHAO: &[u8] = &[0xC4, 0xE3, 0xBA, 0xC3];
    #[cfg(windows)]
    const NIHAO_FILE: &str = "nihao.txt";
    #[cfg(windows)]
    const CP_GBK: u32 = 936;

    fn tmp_dir() -> tempdir::TempDir {
        tempdir::TempDir::new("oven-shell-test").unwrap()
    }

    fn read_marker() -> &'static str {
        #[cfg(windows)]
        {
            "Get-Content -Raw marker.txt"
        }
        #[cfg(not(windows))]
        {
            "cat marker.txt"
        }
    }

    #[cfg(windows)]
    fn ansi_code_page() -> u32 {
        unsafe extern "system" {
            fn GetACP() -> u32;
        }
        // SAFETY: GetACP reads the ANSI code page and has no preconditions.
        unsafe { GetACP() }
    }

    #[test]
    fn bang_prefix_is_shell_input() {
        assert!(ShellInput::parse("!ls").is_some());
        assert!(ShellInput::parse("  !ls").is_some());
        assert!(ShellInput::parse("ls").is_none());
        assert!(ShellInput::parse("/clear").is_none());
        assert!(ShellInput::parse("wow!").is_none());
    }

    #[test]
    fn shell_command_strips_bang_and_whitespace() {
        assert_eq!(
            ShellInput::parse("!ls -la").and_then(|s| s.command()),
            Some("ls -la")
        );
        assert_eq!(
            ShellInput::parse("  !  echo hi").and_then(|s| s.command()),
            Some("echo hi")
        );
        assert_eq!(ShellInput::parse("!").unwrap().command(), None);
        assert_eq!(ShellInput::parse("!   ").unwrap().command(), None);
        assert!(ShellInput::parse("hello").is_none());
    }

    #[test]
    fn envelope_is_not_composer_input() {
        let text = LocalShell {
            command: "ls".into(),
            exit_code: Some(0),
            output: "a.rs".into(),
            error: None,
        }
        .to_string();
        assert!(ShellInput::parse(&text).is_none());
        assert!(LocalShell::try_parse(&text).is_some());
    }

    #[test]
    fn local_shell_ok_requires_zero_exit_and_no_error() {
        assert!(
            LocalShell {
                command: "ls".into(),
                exit_code: Some(0),
                output: String::new(),
                error: None,
            }
            .ok()
        );
        assert!(
            LocalShell {
                command: "ls".into(),
                exit_code: None,
                output: String::new(),
                error: None,
            }
            .ok()
        );
        assert!(
            !LocalShell {
                command: "ls".into(),
                exit_code: Some(1),
                output: String::new(),
                error: None,
            }
            .ok()
        );
        assert!(
            !LocalShell {
                command: "ls".into(),
                exit_code: Some(0),
                output: String::new(),
                error: Some("cancelled".into()),
            }
            .ok()
        );
    }

    #[test]
    fn envelope_round_trip() {
        let shell = LocalShell {
            command: "echo <hi> &".into(),
            exit_code: Some(0),
            output: "a <b> & c".into(),
            error: None,
        };
        let text = shell.to_string();
        let parsed = LocalShell::try_parse(&text).expect("parse");
        assert_eq!(parsed.command, "echo <hi> &");
        assert_eq!(parsed.exit_code, Some(0));
        assert_eq!(parsed.output, "a <b> & c");
        assert_eq!(parsed.error, None);
    }

    #[test]
    fn envelope_omits_exit_on_error() {
        let shell = LocalShell {
            command: "sleep 1".into(),
            exit_code: None,
            output: "partial".into(),
            error: Some("cancelled".into()),
        };
        let parsed = LocalShell::try_parse(&shell.to_string()).unwrap();
        assert_eq!(parsed.exit_code, None);
        assert_eq!(parsed.error.as_deref(), Some("cancelled"));
        assert_eq!(parsed.output, "partial");
    }

    #[test]
    fn ordinary_user_text_is_not_envelope() {
        assert!(LocalShell::try_parse("hello").is_none());
        assert!(LocalShell::try_parse("<local-shell>nope").is_none());
    }

    #[test]
    fn display_line_prefixes_bang() {
        assert_eq!(display_shell_line("ls -la"), "! ls -la");
    }

    #[test]
    fn format_shell_text_empty_is_placeholder() {
        assert_eq!(
            format_shell_text(&ShellOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: Some(0),
            }),
            NO_OUTPUT
        );
    }

    #[tokio::test]
    async fn host_shell_captures_stdout() {
        let tmp = tmp_dir();
        let out = run_host_shell(tmp.path(), "echo hi", HOST_SHELL_TIMEOUT, None)
            .await
            .unwrap();
        assert!(out.stdout.contains("hi"), "{:?}", out.stdout);
        assert_eq!(out.exit_code, Some(0));
        assert!(format_shell_text(&out).contains("hi"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn host_shell_decodes_gbk_chinese_stdout() {
        let tmp = tmp_dir();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join(NIHAO_FILE), GBK_NIHAO).unwrap();
        let command = format!(
            "$b = [IO.File]::ReadAllBytes('{NIHAO_FILE}'); [Console]::OpenStandardOutput().Write($b, 0, $b.Length)"
        );
        let out = run_host_shell(&root, &command, HOST_SHELL_TIMEOUT, None)
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
        let got = out.stdout.trim_end_matches(['\r', '\n']);
        assert_eq!(got, decode_command_output(GBK_NIHAO));
        if ansi_code_page() == CP_GBK {
            assert!(!got.contains(REPLACEMENT), "{got:?}");
            assert_eq!(got, NIHAO);
            assert!(format_shell_text(&out).contains(NIHAO));
        }
    }

    #[tokio::test]
    async fn host_shell_reports_nonzero_exit() {
        let tmp = tmp_dir();
        let out = run_host_shell(tmp.path(), "exit 7", HOST_SHELL_TIMEOUT, None)
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(7));
        assert!(format_shell_text(&out).contains("[exit code: 7]"));
    }

    #[tokio::test]
    async fn host_shell_times_out() {
        let tmp = tmp_dir();
        let err = run_host_shell(tmp.path(), "sleep 5", Duration::from_millis(100), None)
            .await
            .unwrap_err();
        match err {
            ShellError::Timeout { secs, .. } => assert_eq!(secs, 0),
            other => panic!("expected timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn host_shell_runs_in_workspace_root() {
        let tmp = tmp_dir();
        let root: PathBuf = tmp.path().to_path_buf();
        std::fs::write(root.join("marker.txt"), "found").unwrap();
        let out = run_host_shell(&root, read_marker(), HOST_SHELL_TIMEOUT, None)
            .await
            .unwrap();
        assert!(out.stdout.contains("found"), "{:?}", out.stdout);
    }

    #[tokio::test]
    async fn host_shell_cancel_aborts() {
        let tmp = tmp_dir();
        let cancel = CancellationToken::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let root = tmp.path().to_path_buf();
        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            let result = run_host_shell(
                &root,
                "sleep 60",
                HOST_SHELL_TIMEOUT,
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
            .expect("shell task alive");
        assert!(matches!(result, Err(ShellError::Cancelled { .. })));
        handle.await.unwrap();
    }
}

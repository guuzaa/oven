use std::fmt;
use std::time::Duration;

use oven_host::{CommandError, CommandOutput};

pub(crate) const HOST_SHELL_TIMEOUT: Duration = Duration::from_secs(300);
const HISTORY_MAX_LINES: usize = 200;
const HISTORY_MAX_BYTES: usize = 32 * 1024;

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

pub fn display_shell_line(command: &str) -> String {
    format!("{BANG} {command}")
}

pub(crate) fn commit_shell(
    command: &str,
    result: Result<CommandOutput, CommandError>,
) -> LocalShell {
    match result {
        Ok(output) => LocalShell {
            command: command.to_string(),
            exit_code: output.status.and_then(|status| status.code()),
            output: format_shell_text(&output),
            error: None,
        },
        Err(CommandError::Cancelled { output }) => LocalShell {
            command: command.to_string(),
            exit_code: None,
            output: format_shell_text(&output),
            error: Some("cancelled".into()),
        },
        Err(error @ CommandError::TimedOut { .. }) => {
            let message = error.to_string();
            let CommandError::TimedOut { output, .. } = error else {
                unreachable!()
            };
            let mut text = format_shell_text(&output);
            if text == NO_OUTPUT {
                text = message.clone();
            } else if !text.ends_with('\n') {
                text.push('\n');
                text.push_str(&message);
            } else {
                text.push_str(&message);
            }
            LocalShell {
                command: command.to_string(),
                exit_code: None,
                output: text,
                error: Some(message),
            }
        }
        Err(CommandError::Spawn(error)) => shell_error(command, format!("shell: spawn: {error}")),
        Err(CommandError::Wait(error)) => shell_error(command, format!("shell: wait: {error}")),
    }
}

fn shell_error(command: &str, error: String) -> LocalShell {
    LocalShell {
        command: command.to_string(),
        exit_code: None,
        output: error.clone(),
        error: Some(error),
    }
}

fn format_shell_text(output: &CommandOutput) -> String {
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
    if let Some(code) = output.status.and_then(|status| status.code())
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
    use oven_agent::CancellationToken;
    #[cfg(windows)]
    use oven_host::decode_command_output;
    use oven_host::run_shell_command;
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
            format_shell_text(&CommandOutput {
                stdout: String::new(),
                stderr: String::new(),
                status: None,
            }),
            NO_OUTPUT
        );
    }

    #[tokio::test]
    async fn host_shell_captures_stdout() {
        let tmp = tmp_dir();
        let out = run_shell_command("echo hi", tmp.path(), HOST_SHELL_TIMEOUT, None)
            .await
            .unwrap();
        assert!(out.stdout.contains("hi"), "{:?}", out.stdout);
        assert_eq!(out.status.and_then(|status| status.code()), Some(0));
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
        let out = run_shell_command(&command, &root, HOST_SHELL_TIMEOUT, None)
            .await
            .unwrap();
        assert_eq!(out.status.and_then(|status| status.code()), Some(0));
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
        let out = run_shell_command("exit 7", tmp.path(), HOST_SHELL_TIMEOUT, None)
            .await
            .unwrap();
        assert_eq!(out.status.and_then(|status| status.code()), Some(7));
        assert!(format_shell_text(&out).contains("[exit code: 7]"));
    }

    #[tokio::test]
    async fn host_shell_times_out() {
        let tmp = tmp_dir();
        let timeout = Duration::from_millis(100);
        let err = run_shell_command(&sleep_command(5), tmp.path(), timeout, None)
            .await
            .unwrap_err();
        match err {
            CommandError::TimedOut {
                timeout: actual, ..
            } => assert_eq!(actual, timeout),
            other => panic!("expected timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn host_shell_runs_in_workspace_root() {
        let tmp = tmp_dir();
        let root: PathBuf = tmp.path().to_path_buf();
        std::fs::write(root.join("marker.txt"), "found").unwrap();
        let out = run_shell_command(read_marker(), &root, HOST_SHELL_TIMEOUT, None)
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
            let result = run_shell_command(
                &sleep_command(60),
                &root,
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
        assert!(matches!(result, Err(CommandError::Cancelled { .. })));
        handle.await.unwrap();
    }
}

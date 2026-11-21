use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use oven_app::trace::TraceMode;
use oven_app::{App, AppBuilder, AppError, dirs, log, session};

use crate::commands::{self, Command};
use crate::runtime::ui::Ui;

/// What `--trace` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum TraceFlag {
    /// Every word of the run, the text as it streamed.
    Full,
    /// No trace at all.
    Quiet,
}

#[derive(Debug, Parser)]
#[command(name = "oven", about = "A toy coding agent for joy only.")]
#[command(version = concat!(env!("CARGO_PKG_VERSION"), " (",
        env!("GIT_HASH"), " ", env!("GIT_COMMIT_DATE"), " ", env!("BUILD_TARGET"), ")"))]
pub struct Cli {
    /// Tell oven to use the specified directory as its workspace root
    #[arg(long = "cd", short = 'C', default_value = ".", global = true)]
    dir: PathBuf,

    /// Resume / persist a JSONL session id
    #[arg(long, short = 's', env = "OVEN_SESSION")]
    session: Option<String>,

    /// Resume the most recent session for this workspace root
    #[arg(long, short = 'c', conflicts_with = "session")]
    r#continue: bool,

    /// Run a one-shot query and exit
    #[arg(long, short = 'Q', value_name = "QUERY")]
    query: Option<String>,

    /// Run without durable memory: no memory tools, nothing recalled into the prompt
    #[arg(long)]
    amnesia: bool,

    /// What the trace of the run keeps: `full` writes every word of it,
    /// `quiet` writes no trace at all. Without the flag the trace keeps the
    /// shape of the run.
    #[arg(long, value_name = "MODE")]
    trace: Option<TraceFlag>,

    #[command(subcommand)]
    command: Option<Command>,
}

impl Cli {
    /// The session id to use: an explicit `--session` wins; otherwise
    /// `--continue` resumes the most recent session recorded for the
    /// workspace root.
    async fn resolve_session_id(&self) -> Option<String> {
        if let Some(id) = self.session.as_deref() {
            return Some(id.to_string());
        }
        if !self.r#continue {
            return None;
        }
        let dir = dirs::sessions_dir()?;
        match session::recent_session_id(&dir, &self.dir).await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("warning: resolving recent session: {e}");
                None
            }
        }
    }

    /// What the trace of the run keeps.
    fn trace_mode(&self) -> TraceMode {
        match self.trace {
            Some(TraceFlag::Full) => TraceMode::Full,
            Some(TraceFlag::Quiet) => TraceMode::Quiet,
            None => TraceMode::Skeleton,
        }
    }

    async fn builder(&self) -> AppBuilder {
        let mut builder = App::builder(&self.dir);
        if let Err(e) = builder.load_config().await {
            eprintln!("warning: loading config: {e}");
        }
        self.load_memory(&mut builder).await;
        builder.with_trace_mode(self.trace_mode())
    }

    async fn load_memory(&self, builder: &mut AppBuilder) {
        if !self.amnesia {
            builder.load_memory().await;
        }
    }

    async fn query(&self, prompt: &str) -> Result<String, AppError> {
        let mut builder = App::builder(&self.dir);
        builder.load_config().await?;
        self.load_memory(&mut builder).await;
        builder
            .with_trace_mode(self.trace_mode())
            .query(prompt)
            .await
    }

    async fn headless(&self, prompt: &str) -> ExitCode {
        match self.query(prompt).await {
            Ok(resp) => {
                println!("{resp}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                tracing::error!(error = %err, "query failed");
                eprintln!("error: {err}");
                ExitCode::FAILURE
            }
        }
    }

    async fn interactive(&self, session: Option<&str>) -> ExitCode {
        let builder = self.builder().await;
        let app = match builder.open_session(session).await {
            Ok(app) => app,
            Err(err) => {
                tracing::error!(error = %err, "failed to open session");
                eprintln!("error: {err}");
                return ExitCode::FAILURE;
            }
        };

        match Ui::new(app).run().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                tracing::error!(error = %err, "tui failed");
                eprintln!("error: {err}");
                ExitCode::FAILURE
            }
        }
    }

    pub async fn run(&self) -> ExitCode {
        log::init();
        let code = if let Some(command) = &self.command {
            commands::run(command, &self.dir).await
        } else {
            match self.query.as_deref() {
                Some(prompt) => {
                    tracing::info!(root = %self.dir.display(), headless = true, amnesia = self.amnesia, trace = ?self.trace_mode(), "oven starting");
                    self.headless(prompt.trim()).await
                }
                None if io::stdin().is_terminal() && io::stdout().is_terminal() => {
                    let session = self.resolve_session_id().await;
                    tracing::info!(root = %self.dir.display(), headless = false, amnesia = self.amnesia, trace = ?self.trace_mode(), "oven starting");
                    self.interactive(session.as_deref()).await
                }
                None => {
                    eprintln!(
                        "usage: oven [-C DIR] [--session ID] [--continue] [--amnesia] [--trace MODE] [-Q|--query QUERY]"
                    );
                    ExitCode::from(2)
                }
            }
        };
        log::shutdown();
        code
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn the_trace_keeps_the_shape_of_the_run_by_default() {
        let plain = Cli::try_parse_from(["oven"]).unwrap();
        assert_eq!(plain.trace_mode(), TraceMode::Skeleton);
    }

    #[test]
    fn the_trace_flag_asks_for_every_word_or_for_nothing() {
        let full = Cli::try_parse_from(["oven", "--trace", "full"]).unwrap();
        assert_eq!(full.trace_mode(), TraceMode::Full);

        let quiet = Cli::try_parse_from(["oven", "--trace", "quiet"]).unwrap();
        assert_eq!(quiet.trace_mode(), TraceMode::Quiet);
    }

    #[test]
    fn a_trace_mode_it_does_not_know_is_refused() {
        let err = Cli::try_parse_from(["oven", "--trace", "loud"]).unwrap_err();

        let message = err.to_string();
        assert!(message.contains("full"), "{message}");
        assert!(message.contains("quiet"), "{message}");
    }

    #[test]
    fn a_query_keeps_its_trace_too() {
        let err = Cli::try_parse_from(["oven", "--trace", "full", "-Q", "hi"]).unwrap();

        assert_eq!(err.trace_mode(), TraceMode::Full);
    }
}

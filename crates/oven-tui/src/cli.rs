use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use oven_app::{App, AppBuilder, dirs, log, session};

use crate::ui::Ui;

#[derive(Debug, Parser)]
#[command(name = "oven", about = "A toy coding agent for joy only.")]
#[command(version = concat!(env!("CARGO_PKG_VERSION"), " (",
        env!("GIT_HASH"), " ", env!("GIT_COMMIT_DATE"), ")"))]
pub struct Cli {
    /// Tell oven to use the specified directory as its workspace root
    #[arg(long = "cd", short = 'C', default_value = ".")]
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
}

impl Cli {
    /// The session id to use: an explicit `--session` wins; otherwise
    /// `--continue` resumes the most recent session recorded for the
    /// workspace root.
    fn resolve_session_id(&self) -> Option<String> {
        if let Some(id) = self.session.as_deref() {
            return Some(id.to_string());
        }
        if !self.r#continue {
            return None;
        }
        let dir = dirs::sessions_dir()?;
        match session::recent_session_id(&dir, &self.dir) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("warning: resolving recent session: {e}");
                None
            }
        }
    }

    fn builder(&self) -> AppBuilder {
        let mut builder = App::builder(&self.dir);
        if let Err(e) = builder.load_config() {
            eprintln!("warning: loading config: {}", e);
        }
        builder
    }

    async fn headless(&self, prompt: &str) -> ExitCode {
        match App::query(&self.dir, prompt).await {
            Ok(resp) => {
                println!("{resp}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                tracing::error!(error = %err, "query failed");
                ExitCode::FAILURE
            }
        }
    }

    async fn interactive(&self, session: Option<&str>) -> ExitCode {
        let builder = self.builder();
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
        match self.query.as_deref() {
            Some(prompt) => {
                tracing::info!(root = %self.dir.display(), headless = true, "oven starting");
                self.headless(prompt.trim()).await
            }
            None if io::stdin().is_terminal() && io::stdout().is_terminal() => {
                let session = self.resolve_session_id();
                tracing::info!(root = %self.dir.display(), headless = false, "oven starting");
                self.interactive(session.as_deref()).await
            }
            None => {
                eprintln!("usage: oven [-C DIR] [--session ID] [--continue] [-Q|--query QUERY]");
                ExitCode::from(2)
            }
        }
    }
}

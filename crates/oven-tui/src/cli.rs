use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use oven_app::App;

use crate::ui;

#[derive(Debug, Parser)]
#[command(name = "oven", version, about = "A toy coding agent for joy only.")]
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
        let dir = oven_app::session::default_sessions_dir()?;
        match oven_app::session::recent_session_id(&dir, &self.dir) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("warning: resolving recent session: {e}");
                None
            }
        }
    }

    pub fn spawn(&self) -> App {
        let mut app = App::new(&self.dir);
        if let Err(e) = app.load_config() {
            eprintln!("warning: loading config: {}", e);
        }
        app
    }

    pub async fn run(&self) -> ExitCode {
        let app = self.spawn();

        match self.query.as_deref() {
            Some(prompt) => headless(&app, prompt.trim()).await,
            None if io::stdin().is_terminal() && io::stdout().is_terminal() => {
                let session = self.resolve_session_id();
                interactive(&app, session.as_deref()).await
            }
            None => {
                eprintln!("usage: oven [-C DIR] [--session ID] [--continue] [-Q|--query QUERY]");
                ExitCode::from(2)
            }
        }
    }
}

async fn headless(app: &App, prompt: &str) -> ExitCode {
    let handle = match app.spawn().await {
        Ok(h) => h,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };
    let result = handle.prompt(prompt).await;
    handle.shutdown().await;

    match result {
        Ok(out) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn interactive(app: &App, session: Option<&str>) -> ExitCode {
    let handle = match app.spawn_session(session).await {
        Ok(h) => h,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    match ui::Ui::new(handle).run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

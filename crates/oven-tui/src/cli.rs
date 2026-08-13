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

    /// Run a one-shot query and exit
    #[arg(long, short = 'Q', value_name = "QUERY")]
    query: Option<String>,
}

impl Cli {
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
            Some(prompt) => headless(&app, self.session.as_deref(), prompt.trim()).await,
            None if io::stdin().is_terminal() && io::stdout().is_terminal() => {
                interactive(&app, self.session.as_deref()).await
            }
            None => {
                eprintln!("usage: oven [-C DIR] [--session ID] [-Q|--query QUERY]");
                ExitCode::from(2)
            }
        }
    }
}

async fn headless(app: &App, session: Option<&str>, prompt: &str) -> ExitCode {
    let result = match session {
        Some(sid) => {
            let handle = match app.spawn_session(Some(sid)).await {
                Ok(h) => h,
                Err(err) => {
                    eprintln!("error: {}", err);
                    return ExitCode::FAILURE;
                }
            };
            let out = handle.prompt(prompt).await;
            handle.shutdown().await;
            out
        }
        None => {
            let handle = match app.spawn().await {
                Ok(h) => h,
                Err(err) => {
                    eprintln!("error: {err}");
                    return ExitCode::FAILURE;
                }
            };
            let out = handle.prompt(prompt).await;
            handle.shutdown().await;
            out
        }
    };

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

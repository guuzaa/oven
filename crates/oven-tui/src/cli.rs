use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use oven_app::App;

use crate::ui;

#[derive(Debug, Parser)]
#[command(name = "oven", about = "A toy coding agent for joy only.")]
pub struct Cli {
    /// Workspace root
    #[arg(long, short, default_value = ".")]
    root: PathBuf,

    /// Resume / persist a JSONL session id
    #[arg(long, short = 's', env = "OVEN_SESSION")]
    session: Option<String>,

    /// One-shot prompt words (joined). If omitted on a TTY, opens the interactive UI.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    prompt: Vec<String>,
}

impl Cli {
    pub fn spawn(&self) -> App {
        let mut app = App::new(&self.root);
        if let Err(e) = app.load_config() {
            eprintln!("warning: loading config: {}", e);
        }
        app
    }

    pub async fn run(&self) -> ExitCode {
        let app = self.spawn();

        let joined = {
            let s = self.prompt.join(" ");
            if s.trim().is_empty() { None } else { Some(s) }
        };
        let one_shot = joined.or_else(read_piped_prompt);

        match one_shot {
            Some(prompt) => headless(&app, self.session.as_deref(), prompt.trim()).await,
            None if io::stdin().is_terminal() && io::stdout().is_terminal() => {
                interactive(&app, self.session.as_deref()).await
            }
            None => {
                eprintln!("usage: oven [--root DIR] [--session ID] [prompt...]");
                ExitCode::from(2)
            }
        }
    }
}

fn read_piped_prompt() -> Option<String> {
    if io::stdin().is_terminal() {
        return None;
    }
    let mut buf = String::new();
    if io::stdin().read_to_string(&mut buf).is_ok() && !buf.trim().is_empty() {
        Some(buf)
    } else {
        None
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

//! CLI subcommands: configuration a user manages without starting the TUI.
//! Reading and writing config is `oven-app`'s; what lives here is the asking.

pub(crate) mod mem;
pub(crate) mod model;
mod prompt;

use clap::Subcommand;
use std::path::Path;
use std::process::ExitCode;

use oven_app::AppError;

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// List, add and remove provider models
    Model(model::Args),
    /// List, show, remove and edit saved memories
    Mem(mem::Args),
}

pub(crate) async fn run(command: &Command, root: &Path) -> ExitCode {
    match command {
        Command::Model(args) => model::run(args, root).await,
        Command::Mem(args) => mem::run(args, root).await,
    }
}

/// Prints what a command has to say and gives the shell its exit code.
pub(crate) fn report(result: Result<String, AppError>) -> ExitCode {
    match result {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

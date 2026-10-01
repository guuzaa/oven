//! CLI subcommands: configuration a user manages without starting the TUI.
//! Reading and writing config is `oven-app`'s; what lives here is the asking.

pub(crate) mod model;
mod prompt;

use clap::Subcommand;
use std::path::Path;
use std::process::ExitCode;

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// List, add and remove provider models
    Model(model::Args),
}

pub(crate) async fn run(command: &Command, root: &Path) -> ExitCode {
    match command {
        Command::Model(args) => model::run(args, root).await,
    }
}

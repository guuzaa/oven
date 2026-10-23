//! `oven mem`: list, show, remove and edit saved memories.

mod edit;

use std::path::Path;
use std::process::ExitCode;

use clap::{Args as ClapArgs, Subcommand};
use oven_app::AppError;
use oven_app::memory;

use crate::commands::report;

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: MemCommand,
}

#[derive(Debug, Subcommand)]
enum MemCommand {
    /// List saved memories, newest first
    Ls,
    /// Show one memory
    Show(RefArg),
    /// Remove one memory
    Rm(RefArg),
    /// Open one memory in $VISUAL or $EDITOR
    Edit(RefArg),
}

/// The memory a subcommand acts on.
#[derive(Debug, ClapArgs)]
pub(crate) struct RefArg {
    /// `workspace/<id>`, `user/<id>`, or a bare id when it is unique
    #[arg(value_name = "REF")]
    pub(crate) memory_ref: String,
}

pub(crate) async fn run(args: &Args, root: &Path) -> ExitCode {
    report(execute(args, root).await)
}

async fn execute(args: &Args, root: &Path) -> Result<String, AppError> {
    let store = memory::open(root).await;
    match &args.command {
        MemCommand::Ls => Ok(memory::list(&store).await),
        MemCommand::Show(reference) => memory::show(&store, &reference.memory_ref).await,
        MemCommand::Rm(reference) => memory::remove(&store, &reference.memory_ref).await,
        MemCommand::Edit(reference) => edit::run(&store, reference).await,
    }
}

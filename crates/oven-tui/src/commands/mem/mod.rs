//! `oven mem`: list, show, curate and edit saved memories.

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
    /// List saved memories by layer, newest first
    Ls,
    /// List the drafts that wait for review
    Review,
    /// Show one memory
    Show(RefArg),
    /// Remove one memory (global and project go to .trash/)
    Rm(RefArg),
    /// Move one memory to another layer, reviewing it
    Mv(MoveArgs),
    /// Accept one memory: a draft becomes stable, and it counts as reviewed
    Approve(RefArg),
    /// Open one memory in $VISUAL or $EDITOR
    Edit(RefArg),
}

/// The memory a subcommand acts on.
#[derive(Debug, ClapArgs)]
pub(crate) struct RefArg {
    /// `project/<id>`, `shared/<id>`, `global/<id>`, or a bare id when it is unique
    #[arg(value_name = "REF")]
    pub(crate) memory_ref: String,
}

#[derive(Debug, ClapArgs)]
struct MoveArgs {
    /// `project/<id>`, `shared/<id>`, `global/<id>`, or a bare id when it is unique
    #[arg(value_name = "REF")]
    memory_ref: String,
    /// `project`, `shared` or `global`
    #[arg(value_name = "SCOPE")]
    scope: String,
}

pub(crate) async fn run(args: &Args, root: &Path) -> ExitCode {
    report(execute(args, root).await)
}

async fn execute(args: &Args, root: &Path) -> Result<String, AppError> {
    let store = memory::open(root).await;
    match &args.command {
        MemCommand::Ls => Ok(memory::list(&store).await),
        MemCommand::Review => Ok(memory::review(&store).await),
        MemCommand::Show(reference) => memory::show(&store, &reference.memory_ref).await,
        MemCommand::Rm(reference) => memory::remove(&store, &reference.memory_ref).await,
        MemCommand::Mv(args) => memory::move_to(&store, &args.memory_ref, &args.scope).await,
        MemCommand::Approve(reference) => memory::approve(&store, &reference.memory_ref).await,
        MemCommand::Edit(reference) => edit::run(&store, reference).await,
    }
}

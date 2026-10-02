//! `oven mem`: list, show, remove and edit saved memories.

mod edit;

use std::path::Path;
use std::process::ExitCode;

use clap::{Args as ClapArgs, Subcommand};
use oven_app::AppError;
use oven_app::memory;

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
    Show {
        /// `workspace/<id>`, `user/<id>`, or a bare id when it is unique
        #[arg(value_name = "REF")]
        memory_ref: String,
    },
    /// Remove one memory
    Rm {
        /// `workspace/<id>`, `user/<id>`, or a bare id when it is unique
        #[arg(value_name = "REF")]
        memory_ref: String,
    },
    /// Open one memory in $VISUAL or $EDITOR
    Edit(edit::Args),
}

pub(crate) async fn run(args: &Args, root: &Path) -> ExitCode {
    match execute(args, root).await {
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

async fn execute(args: &Args, root: &Path) -> Result<String, AppError> {
    let store = memory::open(root).await;
    match &args.command {
        MemCommand::Ls => Ok(memory::list(&store).await),
        MemCommand::Show { memory_ref } => memory::show(&store, memory_ref).await,
        MemCommand::Rm { memory_ref } => memory::remove(&store, memory_ref).await,
        MemCommand::Edit(args) => edit::run(&store, args).await,
    }
}

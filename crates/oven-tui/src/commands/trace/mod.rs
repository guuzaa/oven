//! `oven trace`: the runs of this workspace, and what one of them did.

pub(crate) mod render;

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args as ClapArgs, Subcommand};
use oven_app::AppError;
use oven_app::trace;

use crate::commands::report;

const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: TraceCommand,
}

#[derive(Debug, Subcommand)]
enum TraceCommand {
    /// List the runs of this workspace, newest first
    Ls,
    /// Show what one run did, in the order it did it
    Show(ShowArgs),
    /// Remove the trace of one run
    Rm(RmArgs),
    /// Remove the traces that are not worth keeping
    Prune(PruneArgs),
}

#[derive(Debug, ClapArgs)]
struct ShowArgs {
    /// The id of the run
    #[arg(value_name = "ID")]
    id: String,
    /// Include what was said: the text of the turn and its reasoning
    #[arg(long)]
    text: bool,
    /// Show the last N events only
    #[arg(long, value_name = "N")]
    tail: Option<usize>,
}

#[derive(Debug, ClapArgs)]
struct RmArgs {
    /// The id of the run
    #[arg(value_name = "ID")]
    id: String,
}

#[derive(Debug, ClapArgs)]
struct PruneArgs {
    /// Keep the newest N runs and remove the rest
    #[arg(long, value_name = "N")]
    keep: Option<usize>,
    /// Remove the runs that were last written longer ago than this
    #[arg(long, value_name = "DAYS")]
    older_than: Option<u64>,
}

pub(crate) async fn run(args: &Args, root: &Path) -> ExitCode {
    report(execute(args, root).await)
}

async fn execute(args: &Args, root: &Path) -> Result<String, AppError> {
    match &args.command {
        TraceCommand::Ls => Ok(render::list(&trace::list_traces(root).await?)),
        TraceCommand::Show(show) => {
            let trace = trace::read_trace(root, &show.id, show.tail).await?;
            Ok(render::timeline(&trace, show.text))
        }
        TraceCommand::Rm(rm) => {
            trace::remove_trace(root, &rm.id).await?;
            Ok(format!("removed the trace of {}", rm.id))
        }
        TraceCommand::Prune(prune) => {
            if prune.keep.is_none() && prune.older_than.is_none() {
                return Err(AppError::Runtime(
                    "say what to prune: --keep or --older-than".into(),
                ));
            }
            let older_than = prune
                .older_than
                .map(|days| Duration::from_secs(days.saturating_mul(SECONDS_PER_DAY)));
            let removed = trace::prune_traces(root, prune.keep, older_than).await?;
            let plural = if removed == 1 { "trace" } else { "traces" };
            Ok(format!("removed {removed} {plural}"))
        }
    }
}

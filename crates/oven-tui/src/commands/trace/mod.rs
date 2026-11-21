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
    Ls(LsArgs),
    /// Show what one run did, as a tree of agents, turns and steps
    Show(ShowArgs),
    /// Remove the trace of one run
    Rm(RmArgs),
    /// Remove the traces that are not worth keeping
    Prune(PruneArgs),
}

#[derive(Debug, ClapArgs)]
struct LsArgs {
    /// Only the runs of this session
    #[arg(long, value_name = "ID")]
    session: Option<String>,
}

#[derive(Debug, ClapArgs)]
struct ShowArgs {
    /// The id of the run
    #[arg(value_name = "ID", conflicts_with = "session")]
    id: Option<String>,
    /// Every run of this session, oldest first
    #[arg(long, value_name = "ID")]
    session: Option<String>,
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
        TraceCommand::Ls(ls) => {
            let traces = match &ls.session {
                Some(session) => trace::traces_of_session(root, session).await?,
                None => trace::list_traces(root).await?,
            };
            Ok(render::list(&traces))
        }
        TraceCommand::Show(show) => match (&show.id, &show.session) {
            (Some(id), _) => {
                let trace = trace::read_trace(root, id, show.tail).await?;
                Ok(render::tree(&trace, show.text))
            }
            (None, Some(session)) => {
                let traces = trace::read_session(root, session, show.tail).await?;
                Ok(render::session(&traces, show.text))
            }
            (None, None) => Err(AppError::Runtime(
                "say which run: an id, or --session for the runs of a session".into(),
            )),
        },
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
            if prune.keep == Some(0) {
                return Err(AppError::Runtime(
                    "say how many to keep: 0 would remove every trace, and 0 elsewhere means \
                     keep them all — use --older-than to age them out"
                        .into(),
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: TraceCommand,
    }

    #[test]
    fn a_session_names_the_runs_of_that_session() {
        let cli = Cli::try_parse_from(["trace", "ls", "--session", "3e4f5678"]).unwrap();

        let TraceCommand::Ls(ls) = cli.command else {
            panic!("expected a listing");
        };
        assert_eq!(ls.session.as_deref(), Some("3e4f5678"));
    }

    #[test]
    fn a_run_is_still_shown_by_its_id() {
        let cli = Cli::try_parse_from(["trace", "show", "a3f9c2b1d4e7"]).unwrap();

        let TraceCommand::Show(show) = cli.command else {
            panic!("expected a run");
        };
        assert_eq!(show.id.as_deref(), Some("a3f9c2b1d4e7"));
        assert!(show.session.is_none());
    }

    #[test]
    fn a_session_shows_every_run_of_it() {
        let cli = Cli::try_parse_from(["trace", "show", "--session", "3e4f5678"]).unwrap();

        let TraceCommand::Show(show) = cli.command else {
            panic!("expected a session");
        };
        assert!(show.id.is_none());
        assert_eq!(show.session.as_deref(), Some("3e4f5678"));
    }

    #[test]
    fn a_run_and_a_session_cannot_both_be_asked_for() {
        assert!(
            Cli::try_parse_from(["trace", "show", "a3f9c2b1d4e7", "--session", "3e4f5678"])
                .is_err(),
            "a run and a session are two different things"
        );
    }
}

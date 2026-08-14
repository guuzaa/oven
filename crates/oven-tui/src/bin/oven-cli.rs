use clap::Parser;
use oven_tui::Cli;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    Cli::parse().run().await
}

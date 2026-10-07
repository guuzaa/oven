//! `oven model`: inspect and edit the saved provider models.

mod add;
mod ls;
mod rm;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args as ClapArgs, Subcommand};
use oven_app::AppError;
use oven_app::config::AppConfig;

/// The config a subcommand reads, and the files it could come from. Only the
/// user file is ever written; the project file is reported when it overrides.
pub(crate) struct Context {
    pub(crate) user_path: Option<PathBuf>,
    pub(crate) project_path: PathBuf,
    pub(crate) config: AppConfig,
}

impl Context {
    async fn load(root: &Path) -> Result<Self, AppError> {
        let user_path = AppConfig::default_user_config_path();
        let project_path = AppConfig::default_project_config_path(root);
        let config = AppConfig::load(user_path.as_deref(), Some(&project_path)).await?;
        Ok(Self {
            user_path,
            project_path,
            config,
        })
    }

    /// The file every subcommand writes, which only exists on a platform with
    /// a home directory.
    pub(crate) fn user_path(&self) -> Result<&Path, AppError> {
        self.user_path
            .as_deref()
            .ok_or_else(|| AppError::Runtime("no user config directory on this platform".into()))
    }

    /// Whether the project file declares `slug`. It takes precedence over the
    /// user file, so an edit there can be overridden without saying so.
    pub(crate) async fn project_declares(&self, slug: &str) -> bool {
        AppConfig::load_file(&self.project_path)
            .await
            .ok()
            .flatten()
            .is_some_and(|config| config.providers.contains_key(slug))
    }
}

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    #[command(subcommand)]
    command: Option<ModelCommand>,
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    /// List every configured and shipped model
    Ls(ls::Args),
    /// Configure a provider or model, asking for what the flags leave out
    Add(add::Args),
    /// Remove a saved provider or model
    Rm(rm::Args),
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

/// `oven model` with no subcommand is `oven model ls`.
async fn execute(args: &Args, root: &Path) -> Result<String, AppError> {
    let ctx = Context::load(root).await?;
    match args.command.as_ref() {
        Some(ModelCommand::Add(args)) => add::run(&ctx, args).await,
        Some(ModelCommand::Rm(args)) => rm::run(&ctx, args).await,
        Some(ModelCommand::Ls(args)) => ls::run(&ctx, args).await,
        None => ls::run(&ctx, &ls::Args::default()).await,
    }
}

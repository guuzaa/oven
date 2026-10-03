//! `oven mem edit`: open one memory in `$VISUAL` or `$EDITOR`.

use std::process::Command;

use clap::Args as ClapArgs;
use oven_app::AppError;
use oven_app::memory::{self, MemoryStore};

const VISUAL: &str = "VISUAL";
const EDITOR: &str = "EDITOR";
const EDITOR_FAILED: &str = "editor exited unsuccessfully";

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// `workspace/<id>`, `user/<id>`, or a bare id when it is unique
    #[arg(value_name = "REF")]
    memory_ref: String,
}

pub(crate) async fn run(store: &MemoryStore, args: &Args) -> Result<String, AppError> {
    let path = memory::file_path(store, &args.memory_ref).await?;
    let visual = std::env::var(VISUAL).ok();
    let editor = std::env::var(EDITOR).ok();
    let program = memory::editor_program(visual.as_deref(), editor.as_deref())?;
    let status = Command::new(&program)
        .arg(&path)
        .status()
        .map_err(|err| AppError::Runtime(err.to_string()))?;
    if !status.success() {
        return Err(AppError::Runtime(format!("{EDITOR_FAILED}: {status}")));
    }
    Ok(path.display().to_string())
}

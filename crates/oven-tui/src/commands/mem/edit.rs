//! `oven mem edit`: open one memory in `$VISUAL` or `$EDITOR`.

use std::process::Command;

use oven_app::AppError;
use oven_app::memory::{self, MemoryStore};

use super::RefArg;

const VISUAL: &str = "VISUAL";
const EDITOR: &str = "EDITOR";
const EDITOR_FAILED: &str = "editor exited unsuccessfully";

pub(crate) async fn run(store: &MemoryStore, reference: &RefArg) -> Result<String, AppError> {
    let path = memory::file_path(store, &reference.memory_ref).await?;
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

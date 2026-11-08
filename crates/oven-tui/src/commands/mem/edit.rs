//! `oven mem edit`: open one memory in `$VISUAL` or `$EDITOR`.

use std::path::Path;
use std::process::Command;

use oven_app::AppError;
use oven_app::memory::{self, MemoryStore};
use oven_host::changed_lines;

use super::RefArg;

const VISUAL: &str = "VISUAL";
const EDITOR: &str = "EDITOR";
const EDITOR_FAILED: &str = "editor exited unsuccessfully";

pub(crate) async fn run(store: &MemoryStore, reference: &RefArg) -> Result<String, AppError> {
    let path = memory::file_path(store, &reference.memory_ref).await?;
    let before = read(&path);
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
    Ok(report(&path, &before, &read(&path)))
}

/// The file the editor changed, named on the first line, with the lines that
/// changed under it.
fn report(path: &Path, before: &str, after: &str) -> String {
    let diff = changed_lines(before, after);
    if diff.is_empty() {
        return path.display().to_string();
    }
    format!("{}\n{}", path.display(), diff.trim_end_matches('\n'))
}

/// The file as it stands, or empty when it cannot be read.
fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::report;

    const PATH: &str = "/memory/workspace/proxy.md";

    #[test]
    fn a_file_that_did_not_change_is_named_alone() {
        assert_eq!(report(Path::new(PATH), "one\n", "one\n"), PATH);
    }

    #[test]
    fn a_changed_file_names_itself_and_shows_the_change() {
        assert_eq!(
            report(Path::new(PATH), "one\ntwo\n", "one\nthree\n"),
            format!("{PATH}\n- 2:two\n+ 2:three")
        );
    }
}

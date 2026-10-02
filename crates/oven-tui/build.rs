use std::path::{Path, PathBuf};
use std::process::Command;

const UNKNOWN: &str = "unknown";
const HEAD: &str = "HEAD";
const PACKED_REFS: &str = "packed-refs";
const SYMREF_PREFIX: &str = "ref: ";

fn main() {
    println!(
        "cargo:rustc-env=GIT_HASH={}",
        git_text(&["rev-parse", "--short=9", "HEAD"])
    );
    println!(
        "cargo:rustc-env=GIT_COMMIT_DATE={}",
        git_text(&["log", "-1", "--format=%cd", "--date=short"])
    );
    declare_rerun_paths();
}

fn git_text(args: &[&str]) -> String {
    git_output(args).unwrap_or_else(|| UNKNOWN.to_string())
}

fn git_output(args: &[&str]) -> Option<String> {
    let mut cmd = Command::new("git");
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        cmd.current_dir(manifest_dir);
    }
    let output = cmd.args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Watch the files that actually determine `HEAD`'s commit.
///
/// A missing `rerun-if-changed` path is always dirty, and this package has no
/// `.git` of its own (the git dir is the workspace root, or a linked worktree).
/// `HEAD` is usually a symref: commits update the branch ref, or `packed-refs`
/// when that ref is packed, not the `HEAD` file itself.
fn declare_rerun_paths() {
    let Some(head) = git_path(HEAD).filter(|path| path.is_file()) else {
        println!("cargo:rerun-if-changed=build.rs");
        return;
    };
    declare(&head);

    let Ok(contents) = std::fs::read_to_string(&head) else {
        return;
    };
    let Some(ref_name) = contents.trim().strip_prefix(SYMREF_PREFIX) else {
        return;
    };

    if let Some(ref_file) = git_path(ref_name).filter(|path| path.is_file()) {
        declare(&ref_file);
        return;
    }
    if let Some(packed) = git_path(PACKED_REFS).filter(|path| path.is_file()) {
        declare(&packed);
    }
}

fn declare(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
}

fn git_path(spec: &str) -> Option<PathBuf> {
    git_output(&["rev-parse", "--git-path", spec]).map(PathBuf::from)
}

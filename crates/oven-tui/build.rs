use std::env;
use std::path::Path;
use std::process::Command;

const WATCHED_REF_FILES: [&str; 3] = ["HEAD", "refs", "packed-refs"];
const UNKNOWN_TARGET: &str = "unknown";

fn main() {
    let git = |args: &[&str]| -> String {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map_or_else(
                || "unknown".into(),
                |o| String::from_utf8_lossy(&o.stdout).trim().to_string(),
            )
    };

    println!("cargo:rerun-if-env-changed=BUILD_TARGET");

    for ref_file in WATCHED_REF_FILES {
        let path = git(&["rev-parse", "--git-path", ref_file]);
        if Path::new(&path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    println!(
        "cargo:rustc-env=GIT_HASH={}",
        git(&["rev-parse", "--short=9", "HEAD"])
    );
    println!(
        "cargo:rustc-env=GIT_COMMIT_DATE={}",
        git(&["log", "-1", "--format=%cd", "--date=short"])
    );
    println!("cargo:rustc-env=BUILD_TARGET={}", build_target());
}

/// `BUILD_TARGET` comes from release CI; local builds fall back to the
/// target triple Cargo sets for the build script.
fn build_target() -> String {
    ["BUILD_TARGET", "TARGET"]
        .into_iter()
        .find_map(|key| env::var(key).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| UNKNOWN_TARGET.into())
}

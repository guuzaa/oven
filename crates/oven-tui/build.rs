use std::path::Path;
use std::process::Command;

const WATCHED_REF_FILES: [&str; 3] = ["HEAD", "refs", "packed-refs"];

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
}

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use thiserror::Error;

#[derive(Debug)]
pub struct WalkEntry {
    path: PathBuf,
    is_file: bool,
}

impl WalkEntry {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_file(&self) -> bool {
        self.is_file
    }
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct WalkError {
    message: String,
}

pub fn walk_dir(root: impl AsRef<Path>) -> impl Iterator<Item = Result<WalkEntry, WalkError>> {
    let root = root.as_ref();
    let skip_dot_dirs = !root.join(".gitignore").is_file();
    WalkBuilder::new(root)
        .require_git(false)
        .hidden(false)
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_name() == ".git" {
                return false;
            }
            if skip_dot_dirs
                && entry
                    .file_type()
                    .is_some_and(|file_type| file_type.is_dir())
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with('.'))
            {
                return false;
            }
            true
        })
        .build()
        .map(|entry| {
            entry
                .map(|entry| WalkEntry {
                    is_file: entry
                        .file_type()
                        .is_some_and(|file_type| file_type.is_file()),
                    path: entry.into_path(),
                })
                .map_err(|error| WalkError {
                    message: error.to_string(),
                })
        })
}

#[cfg(test)]
mod tests {
    use super::walk_dir;
    use std::fs;
    use std::path::Path;

    fn tmp_dir() -> tempdir::TempDir {
        tempdir::TempDir::new("oven-walk").unwrap()
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn files(root: &Path) -> Vec<String> {
        let mut files: Vec<_> = walk_dir(root)
            .filter_map(Result::ok)
            .filter(|entry| entry.is_file())
            .map(|entry| entry.path().strip_prefix(root).unwrap().to_owned())
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect();
        files.sort();
        files
    }

    #[test]
    fn respects_gitignore_and_dot_file_policy() {
        let tmp = tmp_dir();
        write(tmp.path(), ".gitignore", "skip.txt\n");
        write(tmp.path(), "keep.txt", "x");
        write(tmp.path(), "skip.txt", "x");
        write(tmp.path(), ".secret", "x");
        write(tmp.path(), ".github/keep.txt", "x");
        write(tmp.path(), ".git/config", "x");
        assert_eq!(
            files(tmp.path()),
            [".github/keep.txt", ".gitignore", ".secret", "keep.txt"]
        );
    }

    #[test]
    fn skips_dot_directories_without_gitignore() {
        let tmp = tmp_dir();
        write(tmp.path(), "keep.txt", "x");
        write(tmp.path(), ".hidden/x.txt", "x");
        assert_eq!(files(tmp.path()), ["keep.txt"]);
    }
}

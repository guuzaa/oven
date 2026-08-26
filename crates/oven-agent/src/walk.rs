use std::path::Path;

use ignore::{Walk, WalkBuilder};

pub fn walk_dir(root: impl AsRef<Path>) -> Walk {
    let root = root.as_ref();
    let skip_dot_dirs = !has_gitignore(root);
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
                && entry.file_type().is_some_and(|t| t.is_dir())
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
}

#[inline]
fn has_gitignore(start: &Path) -> bool {
    start.join(".gitignore").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let mut out: Vec<String> = walk_dir(root)
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
            .filter_map(|e| {
                e.path()
                    .strip_prefix(root)
                    .ok()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
            })
            .filter(|p| !p.is_empty())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn gitignore_keeps_dot_dirs() {
        let tmp = tmp_dir();
        let root = tmp.path();
        write(root, ".gitignore", "skip.txt\n");
        write(root, "keep.txt", "x");
        write(root, "skip.txt", "x");
        write(root, ".secret", "x");
        write(root, ".github/ci.yml", "x");
        write(root, ".git/HEAD", "ref");
        let files = files(root);
        assert!(files.contains(&"keep.txt".into()), "{files:?}");
        assert!(files.contains(&".secret".into()), "{files:?}");
        assert!(files.contains(&".github/ci.yml".into()), "{files:?}");
        assert!(!files.contains(&"skip.txt".into()), "{files:?}");
        assert!(!files.iter().any(|p| p.starts_with(".git/")), "{files:?}");
    }

    #[test]
    fn no_gitignore_skips_dot_dirs() {
        let tmp = tmp_dir();
        let root = tmp.path();
        write(root, "keep.txt", "x");
        write(root, ".secret", "x");
        write(root, ".hidden/x.txt", "x");
        write(root, ".git/HEAD", "ref");
        let files = files(root);
        assert!(files.contains(&"keep.txt".into()), "{files:?}");
        assert!(files.contains(&".secret".into()), "{files:?}");
        assert!(!files.contains(&".hidden/x.txt".into()), "{files:?}");
        assert!(!files.iter().any(|p| p.starts_with(".git/")), "{files:?}");
    }
}

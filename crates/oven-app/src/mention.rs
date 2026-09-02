use std::path::Path;
use std::sync::Arc;

use nucleo::{Nucleo, Utf32String};
use nucleo_matcher::Config;
use nucleo_matcher::pattern::{CaseMatching, Normalization};

const SEARCH_LIMIT: usize = 50;

pub struct FileMentions {
    nucleo: Nucleo<String>,
}

impl FileMentions {
    pub fn open(root: impl AsRef<Path>) -> Self {
        Self::from_files(scan(root.as_ref()))
    }

    pub fn from_files(files: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let nucleo = Nucleo::new(Config::DEFAULT.match_paths(), Arc::new(|| {}), Some(1), 1);
        let injector = nucleo.injector();
        for path in files {
            let path = path.into();
            injector.push(path, |item, cols| {
                cols[0] = Utf32String::from(item.as_str());
            });
        }
        Self { nucleo }
    }

    pub fn search(&mut self, query: &str) -> Vec<String> {
        self.search_n(query, SEARCH_LIMIT)
    }

    fn search_n(&mut self, query: &str, limit: usize) -> Vec<String> {
        self.nucleo
            .pattern
            .reparse(0, query, CaseMatching::Smart, Normalization::Smart, false);
        loop {
            let status = self.nucleo.tick(10);
            if !status.running {
                break;
            }
        }
        let snap = self.nucleo.snapshot();
        let n = snap.matched_item_count().min(limit as u32);
        snap.matched_items(..n)
            .map(|item| item.data.clone())
            .collect()
    }
}

fn scan(root: &Path) -> Vec<String> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut files = Vec::new();
    for entry in oven_host::walk_dir(root) {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        files.push(rel.to_string_lossy().replace('\\', "/"));
    }
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn tmp_dir() -> tempdir::TempDir {
        tempdir::TempDir::new("oven-mention").unwrap()
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn scan_skips_gitignore_keeps_dot_dirs() {
        let tmp = tmp_dir();
        let root = tmp.path();
        write(root, "keep.txt", "x");
        write(root, "src/lib.rs", "x");
        write(root, ".gitignore", "skip.txt\n");
        write(root, "skip.txt", "x");
        write(root, ".secret", "x");
        write(root, ".github/ci.yml", "x");
        let files = scan(root);
        assert!(files.contains(&"keep.txt".into()), "{files:?}");
        assert!(files.contains(&"src/lib.rs".into()), "{files:?}");
        assert!(files.contains(&".secret".into()), "{files:?}");
        assert!(files.contains(&".github/ci.yml".into()), "{files:?}");
        assert!(!files.contains(&"skip.txt".into()), "{files:?}");

        let mut mentions = FileMentions::open(root);
        assert_eq!(1, mentions.search("lib").len());
        assert_eq!(1, mentions.search("slr").len());
        assert!(mentions.search("skip.txt").is_empty());
        assert_eq!(1, mentions.search("ci.yml").len());
    }

    #[test]
    fn scan_skips_dot_dirs_without_gitignore() {
        let tmp = tmp_dir();
        let root = tmp.path();
        write(root, "keep.txt", "x");
        write(root, ".secret", "x");
        write(root, ".hidden/x.txt", "x");
        let files = scan(root);
        assert!(files.contains(&"keep.txt".into()), "{files:?}");
        assert!(files.contains(&".secret".into()), "{files:?}");
        assert!(!files.contains(&".hidden/x.txt".into()), "{files:?}");
    }

    #[test]
    fn scan_missing_root_is_empty() {
        assert!(scan(Path::new("/no/such/oven-mention-root")).is_empty());
    }

    #[test]
    fn search_ranks_injected_files() {
        let mut mentions = FileMentions::from_files(["README.md", "src/app.rs", "src/lib.rs"]);
        let hits = mentions.search("lib");
        assert_eq!(hits[0], "src/lib.rs");
        assert!(mentions.search("zzzz").is_empty());
        assert_eq!(mentions.search("").len(), 3);
    }
}

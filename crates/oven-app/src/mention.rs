use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use nucleo::{Injector, Nucleo, Utf32String};
use nucleo_matcher::Config;
use nucleo_matcher::pattern::{CaseMatching, Normalization};

const SEARCH_LIMIT: usize = 50;

pub struct FileMentions {
    nucleo: Nucleo<String>,
    files: Vec<String>,
    root: Option<PathBuf>,
    pending: Option<Receiver<Vec<String>>>,
}

impl FileMentions {
    pub fn open(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        let files = scan(&root);
        Self::new(Some(root), files)
    }

    pub fn from_files(files: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let files: Vec<String> = files.into_iter().map(Into::into).collect();
        Self::new(None, files)
    }

    fn new(root: Option<PathBuf>, files: Vec<String>) -> Self {
        let nucleo = Nucleo::new(Config::DEFAULT.match_paths(), Arc::new(|| {}), Some(1), 1);
        inject(&nucleo.injector(), &files);
        Self {
            nucleo,
            files,
            root,
            pending: None,
        }
    }

    pub fn rescan(&mut self) {
        let Some(root) = self.root.clone() else {
            return;
        };
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(scan(&root));
        });
        self.pending = Some(rx);
    }

    pub fn search(&mut self, query: &str) -> Vec<String> {
        self.apply_pending();
        self.search_n(query, SEARCH_LIMIT)
    }

    fn apply_pending(&mut self) {
        let Some(rx) = &self.pending else {
            return;
        };
        match rx.try_recv() {
            Ok(files) => {
                self.pending = None;
                self.replace_files(files);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.pending = None,
        }
    }

    fn replace_files(&mut self, files: Vec<String>) {
        if files == self.files {
            return;
        }
        self.nucleo.restart(true);
        inject(&self.nucleo.injector(), &files);
        self.files = files;
    }

    #[cfg(test)]
    fn wait_rescan(&mut self) {
        if let Some(rx) = self.pending.take()
            && let Ok(files) = rx.recv()
        {
            self.replace_files(files);
        }
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

fn inject(injector: &Injector<String>, files: &[String]) {
    for path in files {
        injector.push(path.clone(), |item, cols| {
            cols[0] = Utf32String::from(item.as_str());
        });
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
    fn rescan_picks_up_created_and_deleted_files() {
        let tmp = tmp_dir();
        let root = tmp.path();
        write(root, "old.txt", "x");
        let mut mentions = FileMentions::open(root);
        assert!(mentions.search("new.txt").is_empty());

        write(root, "new.txt", "x");
        mentions.rescan();
        mentions.wait_rescan();
        assert_eq!(mentions.search("new.txt"), vec!["new.txt".to_string()]);
        assert_eq!(1, mentions.search("old.txt").len());

        fs::remove_file(root.join("old.txt")).unwrap();
        mentions.rescan();
        mentions.wait_rescan();
        assert!(mentions.search("old.txt").is_empty());
    }

    #[test]
    fn rescan_without_root_is_noop() {
        let mut mentions = FileMentions::from_files(["a.txt"]);
        mentions.rescan();
        mentions.wait_rescan();
        assert_eq!(1, mentions.search("a.txt").len());
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

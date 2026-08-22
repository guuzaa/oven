use std::path::Path;
use std::sync::Arc;

use ignore::WalkBuilder;
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
    for entry in WalkBuilder::new(root).require_git(false).build() {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
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
fn rank(files: &[String], query: &str, limit: usize) -> Vec<String> {
    use nucleo_matcher::Matcher;
    use nucleo_matcher::pattern::{AtomKind, Pattern};

    if query.is_empty() {
        return files.iter().take(limit).cloned().collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    pattern
        .match_list(files.iter().map(|s| s.as_str()), &mut matcher)
        .into_iter()
        .take(limit)
        .map(|(s, _)| s.to_string())
        .collect()
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
    fn scan_skips_gitignore_and_hidden() {
        let tmp = tmp_dir();
        let root = tmp.path();
        write(root, "keep.txt", "x");
        write(root, "src/lib.rs", "x");
        write(root, ".gitignore", "skip.txt\n");
        write(root, "skip.txt", "x");
        write(root, ".secret", "x");
        let files = scan(root);
        assert!(files.contains(&"keep.txt".into()), "{files:?}");
        assert!(files.contains(&"src/lib.rs".into()), "{files:?}");
        assert!(!files.contains(&"skip.txt".into()), "{files:?}");
        assert!(!files.iter().any(|p| p.ends_with(".secret")), "{files:?}");
    }

    #[test]
    fn scan_missing_root_is_empty() {
        assert!(scan(Path::new("/no/such/oven-mention-root")).is_empty());
    }

    #[test]
    fn rank_empty_query_preserves_order() {
        let files = vec!["a.rs".into(), "b.rs".into(), "c.rs".into()];
        assert_eq!(rank(&files, "", 2), vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn rank_fuzzy_prefers_path_match() {
        let files = vec!["README.md".into(), "src/app.rs".into(), "src/lib.rs".into()];
        let hits = rank(&files, "lib", 10);
        assert_eq!(hits[0], "src/lib.rs");
        assert!(!hits.iter().any(|p| p == "README.md"));
    }

    #[test]
    fn rank_no_match_is_empty() {
        let files = vec!["src/lib.rs".into()];
        assert!(rank(&files, "zzzz", 10).is_empty());
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

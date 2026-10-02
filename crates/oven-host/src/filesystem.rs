use std::io;
use std::path::{Component, Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;
use tokio::fs;

const TEMP_MARKER: &str = ".tmp-";
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum PathError {
    #[error("missing path argument")]
    MissingPath,
    #[error("path escapes root: {path}")]
    EscapesRoot { path: String },
}

pub fn resolve_within(root: &Path, rel: &str) -> Result<PathBuf, PathError> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Err(PathError::MissingPath);
    }
    for component in Path::new(rel).components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(PathError::EscapesRoot {
                path: rel.to_owned(),
            });
        }
    }
    Ok(root.join(rel))
}

pub async fn write(path: &Path, content: impl AsRef<[u8]>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(path, content).await
}

pub fn write_atomic(path: &Path, content: impl AsRef<[u8]>) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let temp = atomic_temp_path(path);
    if let Err(err) = std::fs::write(&temp, content) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    Ok(())
}

fn atomic_temp_path(target: &Path) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let temp_name = format!(".{name}{TEMP_MARKER}{}-{n}", process::id());
    match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(temp_name),
        _ => PathBuf::from(temp_name),
    }
}

#[cfg(test)]
mod tests {
    use super::{PathError, resolve_within, write_atomic};
    use std::path::Path;

    #[test]
    fn write_atomic_creates_missing_parents() {
        let tmp = tempdir::TempDir::new("atomic-parents").unwrap();
        let path = tmp.path().join("a").join("b").join("note.md");
        write_atomic(&path, "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn write_atomic_replaces_existing_content() {
        let tmp = tempdir::TempDir::new("atomic-replace").unwrap();
        let path = tmp.path().join("note.md");
        write_atomic(&path, "old").unwrap();
        write_atomic(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn write_atomic_leaves_no_temp_file() {
        let tmp = tempdir::TempDir::new("atomic-clean").unwrap();
        let path = tmp.path().join("note.md");
        write_atomic(&path, "body").unwrap();
        let names = dir_names(tmp.path());
        assert_eq!(names, vec!["note.md".to_owned()]);
    }

    #[test]
    fn write_atomic_temp_name_is_hidden_and_not_markdown() {
        let target = Path::new("/tmp/memories/note.md");
        let temp = super::atomic_temp_path(target);
        let name = temp.file_name().unwrap().to_str().unwrap();
        assert_eq!(temp.parent(), target.parent());
        assert!(name.starts_with('.'));
        assert!(!name.ends_with(".md"));
    }

    #[test]
    fn write_atomic_removes_temp_when_rename_fails() {
        let tmp = tempdir::TempDir::new("atomic-fail").unwrap();
        let path = tmp.path().join("note.md");
        std::fs::create_dir(&path).unwrap();
        assert!(write_atomic(&path, "body").is_err());
        let hidden: Vec<_> = dir_names(tmp.path())
            .into_iter()
            .filter(|name| name.starts_with('.'))
            .collect();
        assert!(hidden.is_empty());
    }

    fn dir_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn rejects_missing_and_parent_paths() {
        let root = Path::new("/workspace");
        assert!(matches!(
            resolve_within(root, ""),
            Err(PathError::MissingPath)
        ));
        assert!(matches!(
            resolve_within(root, "../outside"),
            Err(PathError::EscapesRoot { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_absolute_unix_paths() {
        assert!(matches!(
            resolve_within(Path::new("/workspace"), "/etc/passwd"),
            Err(PathError::EscapesRoot { .. })
        ));
    }

    #[cfg(windows)]
    #[test]
    fn rejects_windows_root_and_prefix_paths() {
        let root = Path::new(r"C:\workspace");
        assert!(resolve_within(root, r"\Windows\system32").is_err());
        assert!(resolve_within(root, r"D:\other").is_err());
    }
}

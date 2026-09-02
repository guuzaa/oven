use std::path::{Component, Path, PathBuf};

use thiserror::Error;
use tokio::fs;

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

pub async fn write(path: &Path, content: impl AsRef<[u8]>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(path, content).await
}

#[cfg(test)]
mod tests {
    use super::{PathError, resolve_within};
    use std::path::Path;

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

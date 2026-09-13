use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const LOG_MAX_FILES: usize = 2;

const BACKUP_SUFFIX: &str = ".1";
const BACKUP_TEMP_SUFFIX: &str = ".tmp";
const LOG_HANDLE_MISSING: &str = "log file handle missing";

#[derive(Clone)]
pub struct RotatingFile {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    path: PathBuf,
    file: Option<File>,
    len: u64,
    max_bytes: u64,
    max_files: usize,
}

impl RotatingFile {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_with(path, LOG_MAX_BYTES, LOG_MAX_FILES)
    }

    pub fn open_with(path: impl AsRef<Path>, max_bytes: u64, max_files: usize) -> io::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                path: path.to_path_buf(),
                file: Some(file),
                len,
                max_bytes,
                max_files: max_files.max(1),
            })),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn backup_path(path: &Path) -> PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(BACKUP_SUFFIX);
    PathBuf::from(raw)
}

fn backup_temp_path(path: &Path) -> PathBuf {
    let mut raw = backup_path(path).into_os_string();
    raw.push(BACKUP_TEMP_SUFFIX);
    PathBuf::from(raw)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

impl Inner {
    fn should_rotate(&self, incoming: u64) -> bool {
        self.len > 0 && self.len.saturating_add(incoming) > self.max_bytes
    }

    fn write_buf(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.should_rotate(buf.len() as u64) {
            self.rotate()?;
        }
        let n = self.file()?.write(buf)?;
        self.len += n as u64;
        Ok(n)
    }

    fn flush_file(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }

    fn file(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
        }
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other(LOG_HANDLE_MISSING))
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
            drop(file);
        }
        if self.max_files >= 2 && self.path.exists() {
            let backup = backup_path(&self.path);
            let backup_temp = backup_temp_path(&self.path);
            let has_backup = backup.exists();

            if has_backup {
                remove_if_exists(&backup_temp)?;
                fs::rename(&backup, &backup_temp)?;
            }

            if let Err(rename_error) = fs::rename(&self.path, &backup) {
                if has_backup && let Err(restore_error) = fs::rename(&backup_temp, &backup) {
                    return Err(io::Error::other(format!(
                        "failed to rotate log file: {rename_error}; failed to restore backup: {restore_error}"
                    )));
                }
                return Err(rename_error);
            }

            if has_backup {
                remove_if_exists(&backup_temp)?;
            }
        }
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&self.path)?,
        );
        self.len = 0;
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.lock().write_buf(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.lock().flush_file()
    }
}

#[cfg(test)]
mod tests {
    use super::{LOG_MAX_FILES, RotatingFile};
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    const TEST_MAX_BYTES: u64 = 8;

    fn tmp() -> tempdir::TempDir {
        tempdir::TempDir::new("oven-rotate").unwrap()
    }

    fn log_path(dir: &tempdir::TempDir) -> PathBuf {
        dir.path().join("oven.log")
    }

    fn backup_path(dir: &tempdir::TempDir) -> PathBuf {
        dir.path().join("oven.log.1")
    }

    fn names(dir: &tempdir::TempDir) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn writes_under_limit_stay_in_one_file() {
        let dir = tmp();
        let path = log_path(&dir);
        let mut file = RotatingFile::open_with(&path, TEST_MAX_BYTES, LOG_MAX_FILES).unwrap();
        file.write_all(b"abcd").unwrap();
        file.flush().unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "abcd");
        assert!(!backup_path(&dir).exists());
    }

    #[test]
    fn crossing_limit_rotates_to_backup() {
        let dir = tmp();
        let path = log_path(&dir);
        let mut file = RotatingFile::open_with(&path, TEST_MAX_BYTES, LOG_MAX_FILES).unwrap();
        file.write_all(b"AAAA").unwrap();
        file.write_all(b"BBBB").unwrap();
        file.write_all(b"CCCC").unwrap();
        file.flush().unwrap();

        assert_eq!(fs::read_to_string(backup_path(&dir)).unwrap(), "AAAABBBB");
        assert_eq!(fs::read_to_string(&path).unwrap(), "CCCC");
    }

    #[test]
    fn third_rotation_deletes_oldest() {
        let dir = tmp();
        let path = log_path(&dir);
        let mut file = RotatingFile::open_with(&path, TEST_MAX_BYTES, LOG_MAX_FILES).unwrap();
        file.write_all(b"AAAA").unwrap();
        file.write_all(b"BBBB").unwrap();
        file.write_all(b"CCCC").unwrap();
        file.write_all(b"DDDD").unwrap();
        file.write_all(b"EEEE").unwrap();
        file.flush().unwrap();

        assert_eq!(names(&dir), ["oven.log", "oven.log.1"]);
        assert_eq!(fs::read_to_string(backup_path(&dir)).unwrap(), "CCCCDDDD");
        assert_eq!(fs::read_to_string(&path).unwrap(), "EEEE");
    }

    #[test]
    fn open_oversized_rotates_on_next_write() {
        let dir = tmp();
        let path = log_path(&dir);
        fs::write(&path, b"XXXXXXXXXXXX").unwrap();
        let mut file = RotatingFile::open_with(&path, TEST_MAX_BYTES, LOG_MAX_FILES).unwrap();
        file.write_all(b"yy").unwrap();
        file.flush().unwrap();

        assert_eq!(
            fs::read_to_string(backup_path(&dir)).unwrap(),
            "XXXXXXXXXXXX"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "yy");
    }

    #[test]
    fn creates_missing_parent_dir() {
        let dir = tmp();
        let path = dir.path().join("nested").join("oven.log");
        let mut file = RotatingFile::open_with(&path, TEST_MAX_BYTES, LOG_MAX_FILES).unwrap();
        file.write_all(b"hi").unwrap();
        file.flush().unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "hi");
    }
}

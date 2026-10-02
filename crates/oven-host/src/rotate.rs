use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

pub const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const LOG_MAX_FILES: usize = 2;

const BACKUP_SUFFIX: &str = ".1";
const BACKUP_TEMP_SUFFIX: &str = ".tmp";
const LOG_HANDLE_MISSING: &str = "log file handle missing";
const LOG_CHANNEL_CAPACITY: usize = 4096;
const WRITER_THREAD_NAME: &str = "oven-log";
const LOG_WRITER_GONE: &str = "log writer thread is gone";
pub const LOG_DROPPED_LINES: &str = "warning: logging: dropped log lines:";
pub const LOG_FLUSH_FAILED: &str = "warning: logging: flush failed:";
const LOG_WRITE_FAILED: &str = "warning: logging: write failed:";
const LOG_WRITER_PANICKED: &str = "log writer thread panicked";
const LOG_WRITE_ZERO: &str = "failed to write whole log buffer";

enum Msg {
    Write(Vec<u8>),
    Flush(Option<SyncSender<()>>),
    Shutdown(Option<SyncSender<()>>),
}

#[derive(Clone)]
pub struct RotatingFile {
    tx: SyncSender<Msg>,
    dropped: Arc<AtomicU64>,
    writer: Arc<Writer>,
}

struct Writer {
    join: Mutex<Option<JoinHandle<()>>>,
    tx: SyncSender<Msg>,
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
        let inner = Inner {
            path: path.to_path_buf(),
            file: Some(file),
            len,
            max_bytes,
            max_files: max_files.max(1),
        };
        let (tx, rx) = mpsc::sync_channel(LOG_CHANNEL_CAPACITY);
        let writer_tx = tx.clone();
        let join = thread::Builder::new()
            .name(WRITER_THREAD_NAME.to_owned())
            .spawn(move || writer_loop(inner, rx))?;
        Ok(Self {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
            writer: Arc::new(Writer {
                join: Mutex::new(Some(join)),
                tx: writer_tx,
            }),
        })
    }

    pub fn sync(&self) -> io::Result<()> {
        let (ack_tx, ack_rx) = mpsc::sync_channel(0);
        self.tx
            .send(Msg::Flush(Some(ack_tx)))
            .map_err(|_| io::Error::new(ErrorKind::BrokenPipe, LOG_WRITER_GONE))?;
        ack_rx
            .recv()
            .map_err(|_| io::Error::new(ErrorKind::BrokenPipe, LOG_WRITER_GONE))?;
        Ok(())
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.writer.shutdown()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Writer {
    fn shutdown(&self) -> io::Result<()> {
        let handle = self
            .join
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(handle) = handle else {
            return Ok(());
        };
        let (ack_tx, ack_rx) = mpsc::sync_channel(0);
        if self.tx.send(Msg::Shutdown(Some(ack_tx))).is_ok() {
            let _ = ack_rx.recv();
        }
        handle
            .join()
            .map_err(|_| io::Error::other(LOG_WRITER_PANICKED))?;
        Ok(())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn writer_loop(mut inner: Inner, rx: Receiver<Msg>) {
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Write(buf) => {
                if let Err(error) = inner.write_all_buf(&buf) {
                    eprintln!("{LOG_WRITE_FAILED} {error}");
                }
            }
            Msg::Flush(ack) => flush_and_ack(&mut inner, ack),
            Msg::Shutdown(ack) => {
                flush_and_ack(&mut inner, ack);
                break;
            }
        }
    }
    if let Err(error) = inner.flush_file() {
        eprintln!("{LOG_FLUSH_FAILED} {error}");
    }
}

fn flush_and_ack(inner: &mut Inner, ack: Option<SyncSender<()>>) {
    if let Err(error) = inner.flush_file() {
        eprintln!("{LOG_FLUSH_FAILED} {error}");
    }
    if let Some(ack) = ack {
        let _ = ack.send(());
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
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

impl Inner {
    fn should_rotate(&self, incoming: u64) -> bool {
        self.len > 0 && self.len.saturating_add(incoming) > self.max_bytes
    }

    fn write_all_buf(&mut self, buf: &[u8]) -> io::Result<()> {
        if self.should_rotate(buf.len() as u64) {
            self.rotate()?;
        }
        let mut written = 0;
        while written < buf.len() {
            match self.file()?.write(&buf[written..]) {
                Ok(0) => return Err(io::Error::new(ErrorKind::WriteZero, LOG_WRITE_ZERO)),
                Ok(n) => written += n,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        self.len += written as u64;
        Ok(())
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
        match self.tx.try_send(Msg::Write(buf.to_vec())) {
            Ok(()) => Ok(buf.len()),
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                Ok(buf.len())
            }
            Err(TrySendError::Disconnected(_)) => {
                Err(io::Error::new(ErrorKind::BrokenPipe, LOG_WRITER_GONE))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{LOG_MAX_BYTES, LOG_MAX_FILES, LOG_WRITER_GONE, RotatingFile};
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::{ErrorKind, Write};
    use std::path::PathBuf;
    use std::thread;

    const TEST_MAX_BYTES: u64 = 8;
    const WRITER_COUNT: usize = 4;
    const LINES_PER_WRITER: usize = 50;

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
        file.sync().unwrap();

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
        file.sync().unwrap();

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
        file.sync().unwrap();

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
        file.sync().unwrap();

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
        file.sync().unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "hi");
    }

    #[test]
    fn sync_makes_writes_visible() {
        let dir = tmp();
        let path = log_path(&dir);
        let mut file = RotatingFile::open_with(&path, TEST_MAX_BYTES, LOG_MAX_FILES).unwrap();
        file.write_all(b"hello").unwrap();
        file.sync().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn concurrent_writers_lose_no_line() {
        let dir = tmp();
        let path = log_path(&dir);
        let file = RotatingFile::open_with(&path, LOG_MAX_BYTES, LOG_MAX_FILES).unwrap();
        let mut handles = Vec::with_capacity(WRITER_COUNT);
        for writer_id in 0..WRITER_COUNT {
            let mut file = file.clone();
            handles.push(thread::spawn(move || {
                for line_id in 0..LINES_PER_WRITER {
                    let line = format!("w{writer_id}-l{line_id}\n");
                    file.write_all(line.as_bytes()).unwrap();
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        file.sync().unwrap();

        let mut expected = BTreeSet::new();
        for writer_id in 0..WRITER_COUNT {
            for line_id in 0..LINES_PER_WRITER {
                expected.insert(format!("w{writer_id}-l{line_id}"));
            }
        }
        let actual: BTreeSet<_> = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn shutdown_is_idempotent() {
        let dir = tmp();
        let path = log_path(&dir);
        let mut file = RotatingFile::open_with(&path, TEST_MAX_BYTES, LOG_MAX_FILES).unwrap();
        file.write_all(b"hi").unwrap();
        file.shutdown().unwrap();
        file.shutdown().unwrap();
        let error = file.sync().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::BrokenPipe);
        assert_eq!(error.to_string(), LOG_WRITER_GONE);
    }
}

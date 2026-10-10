//! Exclusive advisory lock for one session id.
//!
//! The lock is a byte range on `<id>.jsonl.lock`, not on the JSONL itself:
//! rewind and compaction replace the transcript by renaming a new file into
//! place, which would drop a lock held on the old inode. `flock` (Unix) and
//! `LockFileEx` (Windows) both die with the process, so a crash cannot leave
//! a resume blocked. Drop also unlocks explicitly: on macOS, closing the
//! descriptor does not release `flock` while the process is still alive.
//! The holder's pid is the first bytes of the lock file,
//! outside the locked range, so the contender can read it on Windows where a
//! byte-range lock is mandatory.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

const PID_WIDTH: usize = 10;

pub(super) enum Acquire {
    Acquired(SessionLock),
    Held { pid: Option<u32> },
}

#[derive(Debug)]
pub(super) struct SessionLock {
    /// Open for the life of the lock. Drop unlocks it; if the process dies
    /// first, the kernel drops the lock on its own.
    #[cfg_attr(windows, allow(dead_code))]
    file: File,
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;

            // macOS does not release `flock` on close while this process is
            // still alive. `LOCK_UN` does, and it is a no-op if we never locked.
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

pub(super) fn acquire(path: &Path) -> io::Result<Acquire> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match try_lock_exclusive(&file) {
        Ok(()) => {
            let mut lock = SessionLock { file };
            write_pid(&mut lock.file, std::process::id())?;
            Ok(Acquire::Acquired(lock))
        }
        Err(err) if is_contended(&err) => Ok(Acquire::Held {
            pid: read_pid(&mut file),
        }),
        Err(err) => Err(err),
    }
}

fn write_pid(file: &mut File, pid: u32) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let text = format!("{pid:0width$}\n", width = PID_WIDTH);
    file.write_all(text.as_bytes())?;
    file.flush()
}

fn read_pid(file: &mut File) -> Option<u32> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut buf = [0u8; PID_WIDTH + 1];
    let n = file.read(&mut buf).ok()?;
    let text = std::str::from_utf8(&buf[..n]).ok()?;
    let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    let pid = digits.parse::<u32>().ok()?;
    (pid > 0).then_some(pid)
}

fn is_contended(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock || raw_contended(err.raw_os_error())
}

#[cfg(unix)]
fn raw_contended(code: Option<i32>) -> bool {
    matches!(code, Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN)
}

#[cfg(windows)]
fn raw_contended(code: Option<i32>) -> bool {
    /// `ERROR_LOCK_VIOLATION`.
    const LOCK_VIOLATION: i32 = 33;
    code == Some(LOCK_VIOLATION)
}

#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;

    // SAFETY: `file` is open. `LOCK_NB` returns instead of sleeping.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn try_lock_exclusive(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    /// One byte past the first 4 GiB. The pid record lives at offset 0, so a
    /// contender can read it while this byte is locked.
    const LOCK_OFFSET_HIGH: u32 = 1;
    const LOCK_LENGTH: u32 = 1;

    let mut overlapped = OVERLAPPED::default();
    overlapped.Anonymous.Anonymous.Offset = 0;
    overlapped.Anonymous.Anonymous.OffsetHigh = LOCK_OFFSET_HIGH;
    // SAFETY: `file` is open for write, and the overlapped block is zeroed
    // aside from the lock offset. `LOCKFILE_FAIL_IMMEDIATELY` does not wait.
    let ok = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            LOCK_LENGTH,
            0,
            &mut overlapped,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn try_lock_exclusive(_file: &File) -> io::Result<()> {
    compile_error!("session locks need flock or LockFileEx");
}

#[cfg(not(any(unix, windows)))]
fn raw_contended(_code: Option<i32>) -> bool {
    false
}

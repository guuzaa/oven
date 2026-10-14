//! Conversation persistence as JSONL.
//!
//! Each session is one file under `<data_dir>/oven/sessions/<id>.jsonl`. The
//! file holds one JSON record per line — a `Message` or a `TokenUsage`
//! record, each with a Unix-millisecond timestamp — appended as the
//! conversation progresses so a crash never loses already-committed turns
//! beyond the last flushed line. A single `TokenUsage` line is written right
//! after the final assistant message of each user turn, so messages no
//! longer carry per-message usage.
//!
//! Opening a session takes an exclusive advisory lock on `<id>.jsonl.lock`,
//! held until the last clone of that [`Session`] is dropped. The kernel
//! releases the lock if the holder dies, so a crashed process cannot block
//! the next resume. A second open of a held id fails instead of attaching.
//!
//! Reading is backward compatible: lines written by older versions — a bare
//! `Message` or the `{"message": ..., "usage": ...}` envelope — are accepted
//! with timestamp 0, and a non-zero envelope usage becomes a `TokenUsage`
//! record after its message.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use oven_agent::Record;
use oven_llm::{Message, Usage};
use serde::Deserialize;
use thiserror::Error;

const SHORT_SESSION_ID_LEN: usize = 8;
const SESSION_SPAN_NAME: &str = "session";
const SESSION_SPAN_ID_FIELD: &str = "id";
const SESSION_FILE_SUFFIX: &str = ".jsonl";
pub(crate) const SESSION_LOCK_SUFFIX: &str = ".jsonl.lock";

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session io {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("session parse {0} line {1}: {2}")]
    Parse(PathBuf, usize, serde_json::Error),
    #[error("session id '{0}' contains path separators")]
    BadId(String),
    #[error("{}", session_in_use_message(id, *pid))]
    InUse { id: String, pid: Option<u32> },
}

/// What a refused resume says. The CLI prints this on stderr and exits.
pub(crate) fn session_in_use_message(id: &str, pid: Option<u32>) -> String {
    match pid {
        Some(pid) => format!("session {id} is in use by process {pid}"),
        None => format!("session {id} is in use by another process"),
    }
}

/// Absolute, symlink-resolved form of a workspace root, falling back to the
/// raw path when canonicalization fails. Used as the key for both the session
/// meta record and the `cwd_latest.json` index so they match.
#[inline]
pub fn canonical_root(root: &Path) -> String {
    root.canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn short_session_id(id: &str) -> &str {
    match id
        .char_indices()
        .nth_back(SHORT_SESSION_ID_LEN.saturating_sub(1))
    {
        Some((i, _)) => &id[i..],
        None => id,
    }
}

pub(crate) fn session_span(id: Option<&str>) -> tracing::Span {
    match id {
        Some(id) => {
            tracing::info_span!(parent: None, SESSION_SPAN_NAME, id = %short_session_id(id))
        }
        None => tracing::Span::none(),
    }
}

pub(crate) fn current_or_session_span(id: Option<&str>) -> tracing::Span {
    let current = tracing::Span::current();
    if current
        .metadata()
        .is_some_and(|meta| meta.name() == SESSION_SPAN_NAME)
    {
        current
    } else {
        session_span(id)
    }
}

pub(crate) fn record_session_span(id: &str) {
    tracing::Span::current().record(
        SESSION_SPAN_ID_FIELD,
        tracing::field::display(short_session_id(id)),
    );
}

#[derive(Debug, Clone)]
pub struct Session {
    id: String,
    path: PathBuf,
    /// Shared so `Session` clones (the store, a tool) keep one lock alive.
    /// `Mutex` is what makes the file `Send`; it is never locked again.
    #[allow(dead_code)]
    lock: Arc<Mutex<super::session_lock::SessionLock>>,
}

/// Legacy JSONL line written by older versions: a message plus the usage its
/// response consumed. Still read back so existing sessions keep their data.
#[derive(Debug, Deserialize)]
struct RecordLine {
    message: Message,
    #[serde(default)]
    usage: Usage,
}

fn session_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}{SESSION_FILE_SUFFIX}"))
}

fn lock_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}{SESSION_LOCK_SUFFIX}"))
}

impl Session {
    /// Open the session for `id` and hold its lock until the last clone is
    /// dropped. The transcript file is created lazily on the first append.
    /// Fails with [`SessionError::InUse`] when another live process already
    /// holds this id.
    pub async fn open(dir: &Path, id: &str) -> Result<Self, SessionError> {
        validate_id(id)?;
        fs::create_dir_all(dir)
            .await
            .map_err(|e| SessionError::Io(dir.to_path_buf(), e))?;
        let lock_at = lock_path(dir, id);
        let lock = match super::session_lock::acquire(&lock_at)
            .map_err(|e| SessionError::Io(lock_at, e))?
        {
            super::session_lock::Acquire::Acquired(lock) => lock,
            super::session_lock::Acquire::Held { pid } => {
                tracing::warn!(session_id = %id, pid = ?pid, "session already open");
                return Err(SessionError::InUse {
                    id: id.to_string(),
                    pid,
                });
            }
        };
        Ok(Self {
            id: id.to_string(),
            path: session_path(dir, id),
            lock: Arc::new(Mutex::new(lock)),
        })
    }

    /// Resume `id` when that transcript already exists; otherwise start a new
    /// uuid v7 session. An explicit id is locked only when its file exists,
    /// so a miss does not leave a lock behind for an id this call discards.
    pub async fn resolve(dir: &Path, id: Option<&str>) -> Result<Self, SessionError> {
        if let Some(id) = id {
            validate_id(id)?;
            let path = session_path(dir, id);
            if fs::try_exists(&path).await.unwrap_or(false) {
                return Self::open(dir, id).await;
            }
        }

        let uuid = uuid::Uuid::now_v7().to_string();
        Self::open(dir, &uuid).await
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read all records (messages and token usage) from disk. Returns an
    /// empty Vec if the file does not yet exist. Accepts the current
    /// `Record` format, the legacy `{"message": ..., "usage": ...}` envelope
    /// (a non-zero usage becomes a `TokenUsage` record after its message),
    /// and legacy bare-`Message` lines (timestamp 0).
    pub async fn load_records(&self) -> Result<Vec<Record>, SessionError> {
        let file = match fs::File::open(&self.path).await {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(SessionError::Io(self.path.clone(), e)),
        };
        let mut lines = BufReader::new(file).lines();
        let mut out = Vec::new();
        let mut line_no = 0usize;
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| SessionError::Io(self.path.clone(), e))?
        {
            line_no += 1;
            if line.trim().is_empty() {
                continue;
            }
            let records = parse_line(&line)
                .map_err(|e| SessionError::Parse(self.path.clone(), line_no, e))?;
            out.extend(records);
        }
        Ok(out)
    }

    /// Append many records (messages and token usage) in one open/flush
    /// cycle.
    pub async fn append_records(&self, records: &[Record]) -> Result<(), SessionError> {
        if records.is_empty() {
            return Ok(());
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
            .map_err(|e| SessionError::Io(self.path.clone(), e))?;
        file.write_all(encode_records(records).as_bytes())
            .await
            .map_err(|e| SessionError::Io(self.path.clone(), e))?;
        file.flush()
            .await
            .map_err(|e| SessionError::Io(self.path.clone(), e))
    }

    /// Replace the entire session file with `records`. Used by rewind, which
    /// truncates the persisted conversation together with its usage. The new
    /// bytes land in a unique temporary file and are renamed into place, so
    /// a crash leaves the previous transcript intact.
    pub async fn overwrite(&self, records: &[Record]) -> Result<(), SessionError> {
        oven_host::write_atomic(&self.path, encode_records(records).as_bytes())
            .await
            .map_err(|e| SessionError::Io(self.path.clone(), e))
    }
}

struct SharedSession {
    session: Session,
    has_content: bool,
}

#[derive(Clone)]
pub(crate) struct SessionStore {
    pub(crate) dir: PathBuf,
    pub(crate) root: String,
    shared: Arc<Mutex<SharedSession>>,
}

impl SessionStore {
    pub(crate) fn new(session: Session, root: &Path, has_content: bool) -> Self {
        let dir = session
            .path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Self {
            dir,
            root: canonical_root(root),
            shared: Arc::new(Mutex::new(SharedSession {
                session,
                has_content,
            })),
        }
    }

    pub(crate) fn current(&self) -> Session {
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .session
            .clone()
    }

    pub(crate) fn set_current(&self, session: Session) {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        shared.session = session;
        shared.has_content = false;
    }

    pub(crate) fn mark_content(&self, has_content: bool) {
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .has_content = has_content;
    }

    pub(crate) fn session_id(&self) -> Option<String> {
        let shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        shared.has_content.then(|| shared.session.id().to_string())
    }

    /// The session file that is current, including one `/clear` just opened
    /// that has no messages yet.
    pub(crate) fn current_id(&self) -> String {
        self.current().id().to_string()
    }
}

/// `cwd_latest.json`: a map of canonical workspace root to the session id most
/// recently used there. Kept separate from the session files so a `/continue`
/// can resolve "which session did I last use in this directory?" with a single
/// read instead of scanning every session.
fn recent_path(dir: &Path) -> PathBuf {
    dir.join("cwd_latest.json")
}

async fn load_recent(dir: &Path) -> Result<BTreeMap<String, String>, SessionError> {
    let path = recent_path(dir);
    match fs::read_to_string(&path).await {
        Ok(text) => serde_json::from_str(&text).map_err(|e| SessionError::Parse(path, 1, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::default()),
        Err(e) => Err(SessionError::Io(path, e)),
    }
}

async fn save_recent(dir: &Path, map: &BTreeMap<String, String>) -> Result<(), SessionError> {
    let path = recent_path(dir);
    let text = serde_json::to_string_pretty(map).expect("recent map serialization cannot fail");
    oven_host::write_atomic(&path, text.as_bytes())
        .await
        .map_err(|e| SessionError::Io(path, e))
}

/// Remember that `session_id` is the most recent session used in `root`.
pub async fn record_recent(dir: &Path, root: &Path, session_id: &str) -> Result<(), SessionError> {
    let mut map = load_recent(dir).await?;
    map.insert(canonical_root(root), session_id.to_string());
    save_recent(dir, &map).await
}

/// The most recent session id recorded for `root`, if any.
pub async fn recent_session_id(dir: &Path, root: &Path) -> Result<Option<String>, SessionError> {
    Ok(load_recent(dir).await?.get(&canonical_root(root)).cloned())
}

/// Parse one JSONL line into records.
///
/// Keep in sync with `oven_agent::Record` variants. Unknown tags are skipped
/// so a newer session file still loads on this version.
const KNOWN_RECORD_TYPES: &[&str] = &[
    "message",
    "token_usage",
    "thinking",
    "session_meta",
    "todo_list",
];

/// 1. Invalid JSON → Err.
/// 2. Object with a known `type` tag → deserialize as `Record` (malformed = Err).
/// 3. Object with an unknown `type` tag → skip.
/// 4. No `type`: legacy envelope, then bare `Message`. Both fail → Err.
fn parse_line(line: &str) -> Result<Vec<Record>, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    if let Some(tag) = value.get("type").and_then(|t| t.as_str()) {
        return if KNOWN_RECORD_TYPES.contains(&tag) {
            serde_json::from_value(value).map(|r| vec![r])
        } else {
            Ok(vec![])
        };
    }
    match serde_json::from_value::<RecordLine>(value.clone()) {
        Ok(envelope) => {
            let mut out = vec![Record::Message {
                timestamp: 0,
                message: envelope.message,
            }];
            if envelope.usage != Usage::default() {
                out.push(Record::TokenUsage {
                    timestamp: 0,
                    usage: envelope.usage,
                });
            }
            Ok(out)
        }
        Err(_) => serde_json::from_value::<Message>(value).map(|message| {
            vec![Record::Message {
                timestamp: 0,
                message,
            }]
        }),
    }
}

fn encode_records(records: &[Record]) -> String {
    let mut out = String::new();
    for record in records {
        let line = serde_json::to_string(record).expect("record serialization cannot fail");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn validate_id(id: &str) -> Result<(), SessionError> {
    if id.is_empty() || id.contains('/') || id.contains('\\') || id == "." || id == ".." {
        return Err(SessionError::BadId(id.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oven_agent::SessionMeta;
    use oven_llm::ContentBlock;

    fn tmp() -> tempdir::TempDir {
        tempdir::TempDir::new("oven-session").unwrap()
    }

    fn assert_no_temp_files(dir: &Path) {
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn current_or_session_span_reuses_open_session_span() {
        let _guard =
            tracing::subscriber::set_default(tracing_subscriber::fmt().with_test_writer().finish());
        let outer = session_span(Some("0193c2a1-b4d5-7e8f-9a0b-1c2d3e4f5678"));
        let inner = outer.in_scope(|| current_or_session_span(Some("other-id")));
        assert_eq!(outer.id(), inner.id());
    }

    #[test]
    fn short_session_id_takes_last_eight_chars() {
        assert_eq!(
            short_session_id("0193c2a1-b4d5-7e8f-9a0b-1c2d3e4f5678"),
            "3e4f5678"
        );
        assert_eq!(short_session_id("12345678"), "12345678");
        assert_eq!(short_session_id("123456789"), "23456789");
        assert_eq!(short_session_id("abc"), "abc");
        assert_eq!(short_session_id(""), "");
        assert_eq!(short_session_id("x会话abcdef"), "会话abcdef");
    }

    fn message_record(timestamp: u64, message: Message) -> Record {
        Record::Message { timestamp, message }
    }

    fn usage_record(timestamp: u64, usage: Usage) -> Record {
        Record::TokenUsage { timestamp, usage }
    }

    #[tokio::test]
    async fn load_missing_returns_empty() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "missing").await.unwrap();
        assert!(session.load_records().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn overwrite_truncates() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        session
            .append_records(&[
                message_record(1, Message::user_text("a")),
                message_record(2, Message::user_text("b")),
                message_record(3, Message::user_text("c")),
            ])
            .await
            .unwrap();
        session
            .overwrite(&[message_record(1, Message::user_text("only"))])
            .await
            .unwrap();
        assert_eq!(session.load_records().await.unwrap().len(), 1);
        assert_no_temp_files(tmp.path());
    }

    #[tokio::test]
    async fn overwrite_never_publishes_a_partial_transcript() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        session
            .overwrite(&[message_record(0, Message::user_text("seed"))])
            .await
            .unwrap();
        let path = session.path().to_path_buf();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_reader = Arc::clone(&stop);
        let reader = tokio::task::spawn_blocking(move || {
            let mut bad = Vec::new();
            while !stop_reader.load(Ordering::Relaxed) {
                match std::fs::read_to_string(&path) {
                    Ok(text) if text.is_empty() => {
                        bad.push("empty".to_string());
                        break;
                    }
                    Ok(text) => {
                        for (i, line) in text.lines().filter(|line| !line.is_empty()).enumerate() {
                            if serde_json::from_str::<serde_json::Value>(line).is_err() {
                                bad.push(format!("line {i}: {line}"));
                                break;
                            }
                        }
                        if !bad.is_empty() {
                            break;
                        }
                    }
                    // Windows replace can briefly hide the destination.
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        bad.push(err.to_string());
                        break;
                    }
                }
            }
            bad
        });
        for n in 0..40u64 {
            session
                .overwrite(&[message_record(
                    n,
                    Message::user_text(format!("body-{n}-{}", "x".repeat(64))),
                )])
                .await
                .unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        let bad = reader.await.unwrap();
        assert!(bad.is_empty(), "{bad:?}");
        let loaded = session.load_records().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_no_temp_files(tmp.path());
    }

    #[tokio::test]
    async fn a_second_open_reports_the_holder_and_drop_releases_the_lock() {
        let tmp = tmp();
        let held = Session::open(tmp.path(), "s1").await.unwrap();
        let err = Session::open(tmp.path(), "s1").await.unwrap_err();
        assert_eq!(
            err.to_string(),
            session_in_use_message("s1", Some(std::process::id()))
        );
        drop(held);
        Session::open(tmp.path(), "s1").await.unwrap();
    }

    const HOLDER_ID: &str = "held-session";
    const HOLDER_ENV: &str = "OVEN_SESSION_LOCK_HOLDER";
    const HOLDER_READY: &str = "ready";
    const HOLDER_TEST: &str = "core::session::tests::session_lock_holder_child";
    const HOLDER_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    const LOCK_RELEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    #[tokio::test]
    async fn session_lock_holder_child() {
        let Ok(dir) = std::env::var(HOLDER_ENV) else {
            return;
        };
        let dir = PathBuf::from(dir);
        let _session = Session::open(&dir, HOLDER_ID).await.unwrap();
        std::fs::write(dir.join(HOLDER_READY), std::process::id().to_string()).unwrap();
        let mut byte = [0u8; 1];
        let _ = std::io::Read::read(&mut std::io::stdin(), &mut byte);
    }

    #[tokio::test]
    async fn a_second_process_cannot_open_a_held_session() {
        let tmp = tmp();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .env(HOLDER_ENV, tmp.path())
            .args(["--exact", HOLDER_TEST])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let stderr_text = Arc::new(std::sync::Mutex::new(String::new()));
        let stderr_slot = Arc::clone(&stderr_text);
        std::thread::spawn(move || {
            let mut captured = String::new();
            let _ = std::io::Read::read_to_string(&mut { stderr }, &mut captured);
            *stderr_slot.lock().unwrap() = captured;
        });

        let ready = tmp.path().join(HOLDER_READY);
        let started = std::time::Instant::now();
        while !ready.exists() {
            if started.elapsed() > HOLDER_START_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "holder did not lock the session: {}",
                    stderr_text.lock().unwrap()
                );
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!(
                    "holder exited early ({status}): {}",
                    stderr_text.lock().unwrap()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let err = Session::open(tmp.path(), HOLDER_ID).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            session_in_use_message(HOLDER_ID, Some(child.id()))
        );

        child.kill().unwrap();
        let _ = child.wait();
        let released = std::time::Instant::now();
        loop {
            match Session::open(tmp.path(), HOLDER_ID).await {
                Ok(_) => break,
                Err(SessionError::InUse { .. }) if released.elapsed() < LOCK_RELEASE_TIMEOUT => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(err) => panic!("{err}"),
            }
        }
    }

    #[tokio::test]
    async fn records_roundtrip_preserves_usage_and_timestamps() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let usage = Usage {
            input_tokens: 123,
            output_tokens: 45,
            cache_read_tokens: 6,
            reasoning_tokens: 7,
        };
        session
            .append_records(&[
                message_record(11, Message::user_text("hello")),
                message_record(22, Message::assistant(vec![ContentBlock::text("hi")])),
                usage_record(22, usage),
            ])
            .await
            .unwrap();

        let loaded = session.load_records().await.unwrap();
        assert_eq!(loaded.len(), 3);
        match (&loaded[0], &loaded[1], &loaded[2]) {
            (
                Record::Message {
                    timestamp: t1,
                    message,
                },
                Record::Message {
                    timestamp: t2,
                    message: assistant,
                },
                Record::TokenUsage {
                    timestamp: t3,
                    usage: u,
                },
            ) => {
                assert_eq!(*t1, 11);
                assert_eq!(message.role, oven_llm::Role::User);
                assert_eq!(*t2, 22);
                assert_eq!(assistant.role, oven_llm::Role::Assistant);
                assert_eq!(*t3, 22);
                assert_eq!(*u, usage);
            }
            _ => panic!("unexpected record kinds"),
        }
    }

    #[tokio::test]
    async fn load_accepts_legacy_bare_message_lines() {
        use std::io::Write;

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let mut file = std::fs::File::create(session.path()).unwrap();
        writeln!(
            file,
            "{}",
            serde_json::to_string(&Message::user_text("old")).unwrap()
        )
        .unwrap();
        file.flush().unwrap();

        let loaded = session.load_records().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(
            matches!(&loaded[0], Record::Message { timestamp: 0, message } if message.role == oven_llm::Role::User)
        );
    }

    #[tokio::test]
    async fn load_accepts_legacy_envelope_lines() {
        use std::io::Write;

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let mut file = std::fs::File::create(session.path()).unwrap();
        // Non-zero usage: expands into a message plus a token-usage record.
        writeln!(
            file,
            r#"{{"message":{},"usage":{{"input_tokens":12,"output_tokens":3}}}}"#,
            serde_json::to_string(&Message::assistant(vec![ContentBlock::text("hi")])).unwrap()
        )
        .unwrap();
        // Zero usage: just the message.
        writeln!(
            file,
            r#"{{"message":{},"usage":{{"input_tokens":0,"output_tokens":0}}}}"#,
            serde_json::to_string(&Message::user_text("plain")).unwrap()
        )
        .unwrap();
        file.flush().unwrap();

        let loaded = session.load_records().await.unwrap();
        assert_eq!(loaded.len(), 3);
        match (&loaded[0], &loaded[1]) {
            (
                Record::Message {
                    timestamp: 0,
                    message,
                },
                Record::TokenUsage {
                    timestamp: 0,
                    usage,
                },
            ) => {
                assert_eq!(message.role, oven_llm::Role::Assistant);
                assert_eq!(usage.input_tokens, 12);
                assert_eq!(usage.output_tokens, 3);
            }
            _ => panic!("expected envelope expansion"),
        }
        assert!(
            matches!(&loaded[2], Record::Message { timestamp: 0, message } if message.role == oven_llm::Role::User)
        );
    }

    #[tokio::test]
    async fn bad_id_rejected() {
        let tmp = tmp();
        assert!(Session::open(tmp.path(), "../escape").await.is_err());
        assert!(Session::open(tmp.path(), "a/b").await.is_err());
        assert!(Session::open(tmp.path(), "").await.is_err());
        assert!(
            Session::resolve(tmp.path(), Some("../escape"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn session_meta_record_roundtrips() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let meta = SessionMeta {
            root: "/ws".into(),
            created_at: 123,
        };
        session
            .append_records(&[
                Record::SessionMeta(meta.clone()),
                message_record(1, Message::user_text("hello")),
            ])
            .await
            .unwrap();

        let loaded = session.load_records().await.unwrap();
        assert!(
            matches!(&loaded[0], Record::SessionMeta(m) if m == &meta),
            "meta must parse from the first line"
        );
    }

    #[tokio::test]
    async fn recent_index_records_latest_session_per_root() {
        let tmp = tmp();
        let root = PathBuf::from("/ws");
        assert_eq!(recent_session_id(tmp.path(), &root).await.unwrap(), None);

        record_recent(tmp.path(), &root, "s1").await.unwrap();
        assert_eq!(
            recent_session_id(tmp.path(), &root)
                .await
                .unwrap()
                .as_deref(),
            Some("s1")
        );

        record_recent(tmp.path(), &root, "s2").await.unwrap();
        assert_eq!(
            recent_session_id(tmp.path(), &root)
                .await
                .unwrap()
                .as_deref(),
            Some("s2"),
            "latest recording wins"
        );

        assert_eq!(
            recent_session_id(tmp.path(), Path::new("/other"))
                .await
                .unwrap(),
            None
        );

        assert!(tmp.path().join("cwd_latest.json").exists());
        assert_no_temp_files(tmp.path());
    }

    #[tokio::test]
    async fn concurrent_recent_saves_leave_parseable_json() {
        let tmp = tmp();
        let dir = tmp.path().to_path_buf();
        let left = {
            let dir = dir.clone();
            tokio::spawn(async move {
                for i in 0..30 {
                    record_recent(&dir, Path::new("/ws-a"), &format!("a{i}"))
                        .await
                        .unwrap();
                }
            })
        };
        let right = {
            let dir = dir.clone();
            tokio::spawn(async move {
                for i in 0..30 {
                    record_recent(&dir, Path::new("/ws-b"), &format!("b{i}"))
                        .await
                        .unwrap();
                }
            })
        };
        left.await.unwrap();
        right.await.unwrap();

        let text = tokio::fs::read_to_string(dir.join("cwd_latest.json"))
            .await
            .unwrap();
        let map: BTreeMap<String, String> = serde_json::from_str(&text).unwrap();
        assert!(!map.is_empty(), "{text}");
        assert_no_temp_files(&dir);
    }

    #[tokio::test]
    async fn thinking_records_roundtrip() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        session
            .append_records(&[
                message_record(1, Message::user_text("q")),
                message_record(40, Message::assistant(vec![ContentBlock::text("a")])),
                Record::Thinking {
                    timestamp: 12,
                    duration_ms: 1_500,
                },
            ])
            .await
            .unwrap();

        let loaded = session.load_records().await.unwrap();
        assert_eq!(loaded.len(), 3);
        assert!(matches!(
            &loaded[2],
            Record::Thinking {
                timestamp: 12,
                duration_ms: 1_500
            }
        ));
    }

    #[tokio::test]
    async fn unknown_type_is_skipped_and_messages_still_load() {
        use std::io::Write;

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let mut file = std::fs::File::create(session.path()).unwrap();
        writeln!(file, r#"{{"type":"future_widget","payload":1}}"#).unwrap();
        writeln!(
            file,
            "{}",
            serde_json::to_string(&Record::Message {
                timestamp: 1,
                message: Message::user_text("kept"),
            })
            .unwrap()
        )
        .unwrap();
        file.flush().unwrap();

        let loaded = session.load_records().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(
            matches!(&loaded[0], Record::Message { message, .. } if message.role == oven_llm::Role::User)
        );
    }

    #[tokio::test]
    async fn legacy_envelope_still_loads_after_type_skip() {
        use std::io::Write;

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let mut file = std::fs::File::create(session.path()).unwrap();
        writeln!(file, r#"{{"type":"future_widget","payload":1}}"#).unwrap();
        writeln!(
            file,
            r#"{{"message":{},"usage":{{"input_tokens":4,"output_tokens":1}}}}"#,
            serde_json::to_string(&Message::user_text("old")).unwrap()
        )
        .unwrap();
        file.flush().unwrap();

        let loaded = session.load_records().await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(
            matches!(&loaded[0], Record::Message { timestamp: 0, message } if message.role == oven_llm::Role::User)
        );
        assert!(matches!(
            &loaded[1],
            Record::TokenUsage { usage, .. } if usage.input_tokens == 4
        ));
    }

    #[tokio::test]
    async fn todo_list_records_roundtrip() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        session
            .append_records(&[
                message_record(1, Message::user_text("hello")),
                Record::TodoList {
                    timestamp: 2,
                    items: vec![],
                },
                message_record(3, Message::assistant(vec![ContentBlock::text("hi")])),
            ])
            .await
            .unwrap();

        let loaded = session.load_records().await.unwrap();
        assert_eq!(loaded.len(), 3);
        assert!(matches!(&loaded[1], Record::TodoList { items, .. } if items.is_empty()));
    }

    #[tokio::test]
    async fn appended_records_are_one_line_each() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        session
            .append_records(&[
                message_record(1, Message::user_text("a")),
                message_record(2, Message::user_text("b")),
                usage_record(
                    2,
                    Usage {
                        input_tokens: 1,
                        output_tokens: 2,
                        cache_read_tokens: 0,
                        reasoning_tokens: 0,
                    },
                ),
            ])
            .await
            .unwrap();

        let text = tokio::fs::read_to_string(session.path()).await.unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in lines {
            assert!(parse_line(line).is_ok(), "{line}");
        }
    }

    #[test]
    fn known_type_malformed_is_error() {
        let err = parse_line(r#"{"type":"todo_list"}"#).unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}

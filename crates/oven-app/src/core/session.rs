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
//! Reading is backward compatible: lines written by older versions — a bare
//! `Message` or the `{"message": ..., "usage": ...}` envelope — are accepted
//! with timestamp 0, and a non-zero envelope usage becomes a `TokenUsage`
//! record after its message.
//!
//! The session meta record names the format the file was written in. A file
//! that records a newer format than this build reads is refused rather than
//! half-understood.
//!
//! The meta also names the directory the session was started in, so a session
//! resumed somewhere else can say so. A file written before that was recorded
//! names none, and there is nothing to compare it against.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use oven_llm::Usage;
use oven_proto::{Record, SESSION_FORMAT_VERSION, SESSION_FORMAT_WITHOUT_VERSION, SessionMeta};
use thiserror::Error;

const SHORT_SESSION_ID_LEN: usize = 8;
const SESSION_SPAN_NAME: &str = "session";
const SESSION_SPAN_ID_FIELD: &str = "id";

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session io {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("session parse {0} line {1}: {2}")]
    Parse(PathBuf, usize, serde_json::Error),
    #[error("session id '{0}' contains path separators")]
    BadId(String),
    /// The version as the file writes it (possibly not a valid `u32`, e.g.
    /// `-1`), and the newest format this build reads.
    #[error(
        "session {0} is in format v{1}; this build reads v{first} to v{2}",
        first = SESSION_FORMAT_WITHOUT_VERSION
    )]
    UnsupportedFormat(PathBuf, String, u32),
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
}

impl Session {
    /// Open (or create) the session file for `id`. The file is created lazily
    /// on the first append.
    pub async fn open(dir: &Path, id: &str) -> Result<Self, SessionError> {
        validate_id(id)?;
        fs::create_dir_all(dir)
            .await
            .map_err(|e| SessionError::Io(dir.to_path_buf(), e))?;
        Ok(Self {
            id: id.to_string(),
            path: dir.join(format!("{id}.jsonl")),
        })
    }

    pub async fn resolve(dir: &Path, id: Option<&str>) -> Result<Self, SessionError> {
        if let Some(id) = id {
            let candidate = Self::open(dir, id).await?;
            if fs::try_exists(candidate.path()).await.unwrap_or(false) {
                return Ok(candidate);
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
    ///
    /// A file that records a format newer than this build reads is refused:
    /// its records may mean something this build does not know. The refusal
    /// comes first: the file is refused when its meta is read, before the
    /// records behind it are read at all.
    pub async fn load_records(&self) -> Result<Vec<Record>, SessionError> {
        let file = match fs::File::open(&self.path).await {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(SessionError::Io(self.path.clone(), e)),
        };
        let mut lines = BufReader::new(file).lines();
        let mut out = Vec::new();
        let mut line_no = 0usize;
        // Same rule as `session_format_version`: the first record names the
        // file's format.
        let mut version = SESSION_FORMAT_WITHOUT_VERSION;
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| SessionError::Io(self.path.clone(), e))?
        {
            line_no += 1;
            if line.trim().is_empty() {
                continue;
            }
            let parse_error = |e| SessionError::Parse(self.path.clone(), line_no, e);
            let value: serde_json::Value = serde_json::from_str(&line).map_err(parse_error)?;
            // Peek the version before deserializing, so an unknown format is
            // refused as such rather than as a parse error. Any meta can
            // refuse the file.
            let line_version = match meta_format_version(&value) {
                Some(Err(named)) => {
                    return Err(SessionError::UnsupportedFormat(
                        self.path.clone(),
                        named,
                        SESSION_FORMAT_VERSION,
                    ));
                }
                Some(Ok(named)) => {
                    // A later meta is read in its own format but does not
                    // change the file's.
                    if out.is_empty() {
                        version = named;
                    }
                    named
                }
                None => version,
            };
            out.extend(parse_value(value, line_version).map_err(parse_error)?);
        }
        Ok(out)
    }

    /// Append many records (messages and token usage) in one open/flush
    /// cycle.
    pub(crate) async fn append_records(&self, records: &[Record]) -> Result<(), SessionError> {
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
    /// truncates the persisted conversation together with its usage.
    pub(crate) async fn overwrite(&self, records: &[Record]) -> Result<(), SessionError> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
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
}

struct SharedSession {
    session: Session,
    has_content: bool,
    /// The format the session file is in. A file keeps the format it was
    /// written in, so the records written into it are written in that format.
    format_version: u32,
}

#[derive(Clone)]
pub(crate) struct SessionStore {
    pub(crate) dir: PathBuf,
    pub(crate) root: String,
    shared: Arc<Mutex<SharedSession>>,
}

impl SessionStore {
    pub(crate) fn new(
        session: Session,
        root: &Path,
        has_content: bool,
        format_version: u32,
    ) -> Self {
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
                format_version,
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
        shared.format_version = SESSION_FORMAT_VERSION;
    }

    /// Appends records to the current session file, written in the format
    /// that file is in.
    pub(crate) async fn append_records(&self, records: Vec<Record>) -> Result<(), SessionError> {
        let records = self.records_for_version(records);
        self.current().append_records(&records).await
    }

    /// Rewrites the current session file, written in the format that file is
    /// in.
    pub(crate) async fn overwrite(&self, records: Vec<Record>) -> Result<(), SessionError> {
        let records = self.records_for_version(records);
        self.current().overwrite(&records).await
    }

    fn records_for_version(&self, records: Vec<Record>) -> Vec<Record> {
        let version = self.format_version();
        records
            .into_iter()
            .map(|record| record.into_version(version))
            .collect()
    }

    fn format_version(&self) -> u32 {
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .format_version
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
    let tmp = dir.join("cwd_latest.json.tmp");
    let text = serde_json::to_string_pretty(map).expect("recent map serialization cannot fail");
    fs::write(&tmp, text)
        .await
        .map_err(|e| SessionError::Io(path.clone(), e))?;
    fs::rename(&tmp, &path)
        .await
        .map_err(|e| SessionError::Io(path.clone(), e))
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
/// Keep in sync with `oven_proto::Record` variants. Unknown tags are skipped
/// so a newer session file still loads on this version.
const KNOWN_RECORD_TYPES: &[&str] = &[
    "message",
    "token_usage",
    "thinking",
    "session_meta",
    "todo_list",
];

/// `version` is the format the line was written in.
///
/// 1. Invalid JSON → Err.
/// 2. `type` tag → [`upgrade_line`], then known tag → `Record` (malformed =
///    Err), unknown tag → skip.
/// 3. No `type`: legacy envelope or bare `Message`, read as format 1.
#[cfg(test)]
fn parse_line(line: &str, version: u32) -> Result<Vec<Record>, serde_json::Error> {
    parse_value(serde_json::from_str(line)?, version)
}

/// [`parse_line`] for a line already read as JSON.
fn parse_value(value: serde_json::Value, version: u32) -> Result<Vec<Record>, serde_json::Error> {
    if value.get("type").is_some_and(serde_json::Value::is_string) {
        return Ok(read_typed(value, version)?.into_iter().collect());
    }
    // Untyped lines predate records: rewrite them as format 1 lines so they
    // go through the same upgrade steps.
    let mut out = Vec::new();
    for typed in legacy_as_typed(value)? {
        out.extend(read_typed(typed, SESSION_FORMAT_WITHOUT_VERSION)?);
    }
    // The envelope always carried a usage; a zero one is not a record.
    out.retain(
        |record| !matches!(record, Record::TokenUsage { usage, .. } if *usage == Usage::default()),
    );
    Ok(out)
}

/// A typed line written in `version` as its record; `None` for an unknown
/// type.
fn read_typed(value: serde_json::Value, version: u32) -> Result<Option<Record>, serde_json::Error> {
    // Upgrade before checking the tag: a step may rename it.
    let value = upgrade_line(value, version);
    match value.get("type").and_then(|t| t.as_str()) {
        Some(tag) if KNOWN_RECORD_TYPES.contains(&tag) => serde_json::from_value(value).map(Some),
        _ => Ok(None),
    }
}

/// An untyped legacy line as format 1 lines with timestamp 0.
///
/// The `{"message": ..., "usage": ...}` envelope becomes a message line and,
/// if it has a usage, a usage line; anything else is a bare `Message`.
fn legacy_as_typed(value: serde_json::Value) -> Result<Vec<serde_json::Value>, serde_json::Error> {
    match value {
        serde_json::Value::Object(mut fields) if fields.contains_key("message") => {
            let message = fields.remove("message").unwrap_or_default();
            let mut out = vec![tagged("message", message)?];
            if let Some(usage) = fields.remove("usage") {
                out.push(tagged("token_usage", usage)?);
            }
            Ok(out)
        }
        other => Ok(vec![tagged("message", other)?]),
    }
}

/// The body of a legacy line as a typed line: tagged `tag`, timestamp 0.
fn tagged(tag: &str, body: serde_json::Value) -> Result<serde_json::Value, serde_json::Error> {
    let serde_json::Value::Object(mut fields) = body else {
        return Err(serde::de::Error::custom(format!(
            "a legacy {tag} line must be an object"
        )));
    };
    fields.insert("type".into(), tag.into());
    fields.insert("timestamp".into(), 0.into());
    Ok(serde_json::Value::Object(fields))
}

/// Runs [`upgrade_step`] for each version from `version` up to
/// [`SESSION_FORMAT_VERSION`]. `version` is always readable: other files are
/// refused before parsing.
fn upgrade_line(mut value: serde_json::Value, version: u32) -> serde_json::Value {
    debug_assert!(SessionMeta::format_is_readable_by_this_build(version));
    for from in version..SESSION_FORMAT_VERSION {
        value = upgrade_step(value, from);
    }
    value
}

/// Turns a line in `from` into one in `from + 1`. A format that changes a
/// record's shape adds its step here.
fn upgrade_step(value: serde_json::Value, from: u32) -> serde_json::Value {
    match from {
        // 2 only added an optional field.
        1 => value,
        // Never asked for: `upgrade_line` stops at this build's format.
        _ => value,
    }
}

/// The format a session meta line names, read from the raw JSON: `Ok` if
/// readable, `Err` with the version as written otherwise.
///
/// A missing field means format 1. Any integer, including a negative or
/// oversized one, counts as a version. A non-integer returns `None` and is
/// left to deserialization to reject.
fn meta_format_version(value: &serde_json::Value) -> Option<Result<u32, String>> {
    if value.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
        return None;
    }
    let Some(named) = value.get("format_version") else {
        return Some(Ok(SESSION_FORMAT_WITHOUT_VERSION));
    };
    let serde_json::Value::Number(number) = named else {
        return None;
    };
    if number.as_u64().is_none() && number.as_i64().is_none() {
        return None;
    }
    let readable = number
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|&n| SessionMeta::format_is_readable_by_this_build(n));
    Some(readable.ok_or_else(|| number.to_string()))
}

/// The session's meta: the first record of a file that has one.
fn session_meta(records: &[Record]) -> Option<&SessionMeta> {
    match records.first() {
        Some(Record::SessionMeta(meta)) => Some(meta),
        _ => None,
    }
}

/// The format a session file is in.
///
/// The first record's meta names it (the same rule `Session::load_records`
/// uses). No leading meta means format 1; an empty file is in this build's
/// format.
pub(crate) fn session_format_version(records: &[Record]) -> u32 {
    match session_meta(records) {
        Some(meta) => meta.format_version,
        None if records.is_empty() => SESSION_FORMAT_VERSION,
        None => SESSION_FORMAT_WITHOUT_VERSION,
    }
}

/// The directory a session was started in, if it recorded one.
///
/// A file written before the session recorded its working directory records
/// none: there is nothing to compare it against, so it is ignored.
pub(crate) fn session_working_dir(records: &[Record]) -> Option<&str> {
    session_meta(records)?.working_dir.as_deref()
}

/// What a session says when it is resumed in another directory.
///
/// The paths a conversation names mean something else in another directory,
/// so the user is told which one the session was started in.
pub(crate) fn working_dir_notice(records: &[Record], current: &Path) -> Option<String> {
    let recorded = session_working_dir(records)?;
    let current = canonical_root(current);
    (recorded != current)
        .then(|| format!("session was started in {recorded}, resumed in {current}"))
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
    use oven_llm::{ContentBlock, Message};
    use oven_proto::{SESSION_FORMAT_WITHOUT_VERSION, SessionMeta};

    fn tmp() -> tempdir::TempDir {
        tempdir::TempDir::new("oven-session").unwrap()
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
    }

    #[tokio::test]
    async fn session_meta_record_roundtrips() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let meta = SessionMeta {
            root: "/ws".into(),
            created_at: 123,
            format_version: SESSION_FORMAT_VERSION,
            working_dir: Some("/ws".into()),
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
    async fn a_file_without_a_format_version_reads_as_the_format_that_introduced_it() {
        use std::io::Write;

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let mut file = std::fs::File::create(session.path()).unwrap();
        writeln!(
            file,
            r#"{{"type":"session_meta","root":"/ws","created_at":1}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let loaded = session.load_records().await.unwrap();
        assert!(
            matches!(&loaded[0], Record::SessionMeta(meta) if meta.format_version == SESSION_FORMAT_WITHOUT_VERSION),
            "a file without a version is in the format that introduced it"
        );
    }

    #[tokio::test]
    async fn a_file_from_a_newer_format_is_refused() {
        use std::io::Write;

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let mut file = std::fs::File::create(session.path()).unwrap();
        writeln!(
            file,
            r#"{{"type":"session_meta","root":"/ws","created_at":1,"format_version":99}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let error = session.load_records().await.unwrap_err();
        assert!(
            refused_as(&error, "99"),
            "a file this build cannot read must be refused, got {error}"
        );
    }

    #[tokio::test]
    async fn a_newer_format_is_refused_before_the_records_behind_it_are_read() {
        use std::io::Write;

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let mut file = std::fs::File::create(session.path()).unwrap();
        writeln!(
            file,
            r#"{{"type":"session_meta","root":"/ws","created_at":1,"format_version":99}}"#
        )
        .unwrap();
        // A record this build knows, in a shape this build does not know.
        writeln!(file, r#"{{"type":"todo_list","items":[]}}"#).unwrap();
        file.flush().unwrap();

        let error = session.load_records().await.unwrap_err();
        assert!(
            refused_as(&error, "99"),
            "a newer format is refused as such, not as a parse error: {error}"
        );
    }

    #[tokio::test]
    async fn a_file_in_a_format_below_the_first_is_refused() {
        use std::io::Write;

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let mut file = std::fs::File::create(session.path()).unwrap();
        writeln!(
            file,
            r#"{{"type":"session_meta","root":"/ws","created_at":1,"format_version":0}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"thinking","timestamp":1,"duration_ms":2}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let error = session.load_records().await.unwrap_err();
        assert!(
            refused_as(&error, "0"),
            "no oven wrote format 0, so a file that names it is refused: {error}"
        );
    }

    #[tokio::test]
    async fn a_newer_meta_in_a_shape_this_build_does_not_know_is_refused_as_a_format() {
        use std::io::Write;

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let mut file = std::fs::File::create(session.path()).unwrap();
        // No `root`: this build could not deserialize it.
        writeln!(
            file,
            r#"{{"type":"session_meta","workspace":"/ws","created_at":1,"format_version":99}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let error = session.load_records().await.unwrap_err();
        assert!(
            refused_as(&error, "99"),
            "the version is read before the meta is: {error}"
        );
    }

    #[test]
    fn a_line_reads_the_same_in_every_format_this_build_reads() {
        let line = r#"{"type":"thinking","timestamp":1,"duration_ms":2}"#;
        let expected =
            serde_json::to_value(parse_line(line, SESSION_FORMAT_VERSION).unwrap()).unwrap();
        for version in SESSION_FORMAT_WITHOUT_VERSION..SESSION_FORMAT_VERSION {
            let read = serde_json::to_value(parse_line(line, version).unwrap()).unwrap();
            assert_eq!(read, expected, "v{version}");
        }
    }

    fn refused_as(error: &SessionError, version: &str) -> bool {
        matches!(error, SessionError::UnsupportedFormat(_, named, _) if named == version)
    }

    async fn session_with_lines(tmp: &tempdir::TempDir, lines: &[&str]) -> Session {
        let session = Session::open(tmp.path(), "s").await.unwrap();
        std::fs::write(session.path(), lines.join("\n") + "\n").unwrap();
        session
    }

    #[tokio::test]
    async fn a_file_in_a_negative_format_is_refused_as_that_format() {
        let tmp = tmp();
        let session = session_with_lines(
            &tmp,
            &[r#"{"type":"session_meta","root":"/ws","created_at":1,"format_version":-1}"#],
        )
        .await;

        let error = session.load_records().await.unwrap_err();
        assert!(refused_as(&error, "-1"), "{error}");
    }

    #[tokio::test]
    async fn a_meta_further_down_that_names_a_newer_format_refuses_the_file() {
        let tmp = tmp();
        let session = session_with_lines(
            &tmp,
            &[
                r#"{"type":"session_meta","root":"/ws","created_at":1,"format_version":2}"#,
                r#"{"type":"thinking","timestamp":1,"duration_ms":2}"#,
                r#"{"type":"session_meta","root":"/ws","created_at":1,"format_version":99}"#,
            ],
        )
        .await;

        let error = session.load_records().await.unwrap_err();
        assert!(refused_as(&error, "99"), "{error}");
    }

    #[tokio::test]
    async fn a_meta_behind_a_skipped_line_names_the_files_format() {
        let tmp = tmp();
        let session = session_with_lines(
            &tmp,
            &[
                r#"{"type":"from_a_newer_oven"}"#,
                r#"{"type":"session_meta","root":"/ws","created_at":1,"format_version":2}"#,
            ],
        )
        .await;

        // The skipped line yields no record, so the meta is first.
        let loaded = session.load_records().await.unwrap();
        assert_eq!(session_format_version(&loaded), 2);
    }

    #[test]
    fn a_legacy_line_reads_as_the_typed_lines_of_format_1() {
        let message = r#"{"role":"user","content":[{"type":"text","text":"hi"}]}"#;
        let usage = r#"{"input_tokens":3,"output_tokens":4}"#;

        let legacy = parse_line(
            &format!(r#"{{"message":{message},"usage":{usage}}}"#),
            SESSION_FORMAT_VERSION,
        )
        .unwrap();
        let mut typed = parse_line(
            &message.replacen('{', r#"{"type":"message","timestamp":0,"#, 1),
            SESSION_FORMAT_WITHOUT_VERSION,
        )
        .unwrap();
        typed.extend(
            parse_line(
                &usage.replacen('{', r#"{"type":"token_usage","timestamp":0,"#, 1),
                SESSION_FORMAT_WITHOUT_VERSION,
            )
            .unwrap(),
        );
        assert_eq!(
            serde_json::to_value(&legacy).unwrap(),
            serde_json::to_value(&typed).unwrap()
        );

        let bare = parse_line(message, SESSION_FORMAT_VERSION).unwrap();
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            serde_json::to_value(&typed[..1]).unwrap()
        );
    }

    #[test]
    fn a_legacy_line_that_is_not_an_object_is_an_error() {
        assert!(parse_line("42", SESSION_FORMAT_VERSION).is_err());
        assert!(parse_line(r#"{"message":42}"#, SESSION_FORMAT_VERSION).is_err());
    }

    #[test]
    fn a_meta_names_its_format_from_the_raw_line() {
        let named = serde_json::json!({"type": "session_meta", "format_version": 2});
        assert_eq!(meta_format_version(&named), Some(Ok(2)));

        let unnamed = serde_json::json!({"type": "session_meta", "root": "/ws"});
        assert_eq!(
            meta_format_version(&unnamed),
            Some(Ok(SESSION_FORMAT_WITHOUT_VERSION))
        );

        for (version, written) in [
            (serde_json::json!(7), "7"),
            (serde_json::json!(0), "0"),
            (serde_json::json!(-1), "-1"),
            (serde_json::json!(u64::MAX), "18446744073709551615"),
        ] {
            let meta = serde_json::json!({"type": "session_meta", "format_version": version});
            assert_eq!(
                meta_format_version(&meta),
                Some(Err(written.to_string())),
                "{written}"
            );
        }

        // Not an integer: left to deserialization to reject.
        for version in [serde_json::json!(1.5), serde_json::json!("2")] {
            let meta = serde_json::json!({"type": "session_meta", "format_version": version});
            assert_eq!(meta_format_version(&meta), None, "{version}");
        }

        let other = serde_json::json!({"type": "thinking", "format_version": 7});
        assert_eq!(meta_format_version(&other), None);
    }

    #[test]
    fn a_meta_further_down_does_not_name_the_files_format() {
        let records = vec![
            message_record(1, Message::user_text("old")),
            Record::SessionMeta(SessionMeta {
                root: "/ws".into(),
                created_at: 1,
                format_version: SESSION_FORMAT_VERSION,
                working_dir: Some("/elsewhere".into()),
            }),
        ];
        assert_eq!(
            session_format_version(&records),
            SESSION_FORMAT_WITHOUT_VERSION
        );
        assert!(session_working_dir(&records).is_none());
    }

    #[test]
    fn a_session_resumed_in_another_directory_says_so() {
        let records = vec![Record::SessionMeta(SessionMeta {
            root: "/ws".into(),
            created_at: 1,
            format_version: SESSION_FORMAT_VERSION,
            working_dir: Some("/elsewhere".into()),
        })];

        let notice = working_dir_notice(&records, Path::new("/ws")).unwrap();
        assert!(
            notice.contains("/elsewhere") && notice.contains("/ws"),
            "{notice}"
        );
    }

    #[test]
    fn a_session_resumed_in_the_same_directory_says_nothing() {
        let records = vec![Record::SessionMeta(SessionMeta {
            root: "/ws".into(),
            created_at: 1,
            format_version: SESSION_FORMAT_VERSION,
            working_dir: Some("/ws".into()),
        })];

        assert!(working_dir_notice(&records, Path::new("/ws")).is_none());
    }

    #[test]
    fn a_file_without_a_working_dir_is_ignored() {
        let records = vec![Record::SessionMeta(SessionMeta {
            root: "/ws".into(),
            created_at: 1,
            format_version: SESSION_FORMAT_WITHOUT_VERSION,
            working_dir: None,
        })];

        assert!(session_working_dir(&records).is_none());
        assert!(working_dir_notice(&records, Path::new("/elsewhere")).is_none());
    }

    #[test]
    fn the_format_of_a_file_is_the_one_its_meta_records() {
        let records = vec![Record::SessionMeta(SessionMeta {
            root: "/ws".into(),
            created_at: 1,
            format_version: SESSION_FORMAT_WITHOUT_VERSION,
            working_dir: None,
        })];
        assert_eq!(
            session_format_version(&records),
            SESSION_FORMAT_WITHOUT_VERSION
        );
    }

    #[test]
    fn a_file_without_a_meta_is_in_the_format_that_introduced_the_version() {
        let records = vec![message_record(1, Message::user_text("old"))];
        assert_eq!(
            session_format_version(&records),
            SESSION_FORMAT_WITHOUT_VERSION
        );
    }

    #[test]
    fn an_empty_file_is_in_the_format_this_build_writes() {
        assert_eq!(session_format_version(&[]), SESSION_FORMAT_VERSION);
    }

    #[tokio::test]
    async fn a_session_file_is_written_in_the_format_it_is_in() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let store = SessionStore::new(
            session,
            Path::new("/ws"),
            false,
            SESSION_FORMAT_WITHOUT_VERSION,
        );
        store
            .append_records(vec![Record::SessionMeta(SessionMeta {
                root: "/ws".into(),
                created_at: 1,
                format_version: SESSION_FORMAT_VERSION,
                working_dir: Some("/ws".into()),
            })])
            .await
            .unwrap();

        let text = tokio::fs::read_to_string(store.current().path())
            .await
            .unwrap();
        assert!(
            text.contains(&format!(
                r#""format_version":{SESSION_FORMAT_WITHOUT_VERSION}"#
            )),
            "{text}"
        );
        assert!(!text.contains("working_dir"), "{text}");
    }

    #[tokio::test]
    async fn a_new_session_file_is_written_in_the_format_this_build_writes() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").await.unwrap();
        let store = SessionStore::new(session, Path::new("/ws"), false, SESSION_FORMAT_VERSION);
        store
            .append_records(vec![Record::SessionMeta(SessionMeta {
                root: "/ws".into(),
                created_at: 1,
                format_version: SESSION_FORMAT_VERSION,
                working_dir: Some("/ws".into()),
            })])
            .await
            .unwrap();

        let text = tokio::fs::read_to_string(store.current().path())
            .await
            .unwrap();
        assert!(
            text.contains(&format!(r#""format_version":{SESSION_FORMAT_VERSION}"#)),
            "{text}"
        );
        assert!(text.contains(r#""working_dir":"/ws""#), "{text}");
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
        assert!(!tmp.path().join("cwd_latest.json.tmp").exists());
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
            assert!(parse_line(line, SESSION_FORMAT_VERSION).is_ok(), "{line}");
        }
    }

    #[test]
    fn known_type_malformed_is_error() {
        let err = parse_line(r#"{"type":"todo_list"}"#, SESSION_FORMAT_VERSION).unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}

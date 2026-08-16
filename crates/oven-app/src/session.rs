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

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use oven_agent::{Record, SessionMeta};
use oven_llm::{Message, Usage};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session io {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("session parse {0} line {1}: {2}")]
    Parse(PathBuf, usize, serde_json::Error),
    #[error("session id '{0}' contains path separators")]
    BadId(String),
}

/// Where sessions are stored. Defaults to `$XDG_DATA_HOME/oven/sessions/` (or
/// `~/.local/share/oven/sessions/`).
pub fn default_sessions_dir() -> Option<PathBuf> {
    cross_xdg::BaseDirs::with_prefix("oven")
        .ok()
        .map(|d| d.data_home().join("sessions"))
}

/// Absolute, symlink-resolved form of a workspace root, falling back to the
/// raw path when canonicalization fails. Used as the key for both the session
/// meta record and the `cwd_latest.json` index so they match.
pub fn canonical_root(root: &Path) -> String {
    root.canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

#[derive(Debug, Clone)]
pub struct Session {
    id: String,
    path: PathBuf,
}

/// Legacy JSONL line written by older versions: a message plus the usage its
/// response consumed. Still read back so existing sessions keep their data.
#[derive(Debug, Deserialize)]
struct RecordLine {
    message: Message,
    #[serde(default)]
    usage: Usage,
}

impl Session {
    /// Open (or create) the session file for `id`. The file is created lazily
    /// on the first append.
    pub fn open(dir: &Path, id: &str) -> Result<Self, SessionError> {
        validate_id(id)?;
        if !dir.exists() {
            fs::create_dir_all(dir).map_err(|e| SessionError::Io(dir.to_path_buf(), e))?;
        }
        Ok(Self {
            id: id.to_string(),
            path: dir.join(format!("{id}.jsonl")),
        })
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
    pub fn load_records(&self) -> Result<Vec<Record>, SessionError> {
        match fs::File::open(&self.path) {
            Ok(f) => {
                let reader = BufReader::new(f);
                let mut out = Vec::new();
                for (i, line) in reader.lines().enumerate() {
                    let line = line.map_err(|e| SessionError::Io(self.path.clone(), e))?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    let records = parse_line(&line)
                        .map_err(|e| SessionError::Parse(self.path.clone(), i + 1, e))?;
                    out.extend(records);
                }
                Ok(out)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(SessionError::Io(self.path.clone(), e)),
        }
    }

    /// Read all messages from disk, dropping token-usage records. See
    /// [`load_records`](Self::load_records).
    pub fn load(&self) -> Result<Vec<Message>, SessionError> {
        self.load_records().map(|records| {
            records
                .into_iter()
                .filter_map(|r| match r {
                    Record::Message { message, .. } => Some(message),
                    Record::TokenUsage { .. }
                    | Record::SessionMeta(_)
                    | Record::TodoList { .. } => None,
                })
                .collect()
        })
    }

    /// Read the session's metadata record (its workspace root and creation
    /// time), the first line of the file. `None` for a missing file, an
    /// empty file, or a legacy session that predates meta records.
    pub fn load_meta(&self) -> Result<Option<SessionMeta>, SessionError> {
        let Some(line) = read_first_line(&self.path)? else {
            return Ok(None);
        };
        let records =
            parse_line(&line).map_err(|e| SessionError::Parse(self.path.clone(), 1, e))?;
        Ok(records.into_iter().find_map(|r| match r {
            Record::SessionMeta(meta) => Some(meta),
            _ => None,
        }))
    }

    /// Append many records (messages and token usage) in one open/flush
    /// cycle.
    pub fn append_records(&self, records: &[Record]) -> Result<(), SessionError> {
        if records.is_empty() {
            return Ok(());
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| SessionError::Io(self.path.clone(), e))?;
        for record in records {
            write_record(&mut file, record).map_err(|e| SessionError::Io(self.path.clone(), e))?;
        }
        file.flush()
            .map_err(|e| SessionError::Io(self.path.clone(), e))
    }

    /// Replace the entire session file with `records`. Used by rewind, which
    /// truncates the persisted conversation together with its usage.
    pub fn overwrite(&self, records: &[Record]) -> Result<(), SessionError> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&self.path)
            .map_err(|e| SessionError::Io(self.path.clone(), e))?;
        for record in records {
            write_record(&mut file, record).map_err(|e| SessionError::Io(self.path.clone(), e))?;
        }
        file.flush()
            .map_err(|e| SessionError::Io(self.path.clone(), e))
    }

    /// Delete the session file. Idempotent for missing files.
    pub fn delete(&self) -> Result<(), SessionError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SessionError::Io(self.path.clone(), e)),
        }
    }

    /// List the session ids present in `dir`.
    pub fn list(dir: &Path) -> Result<Vec<String>, SessionError> {
        let mut out = Vec::new();
        let read = match fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(SessionError::Io(dir.to_path_buf(), e)),
        };
        for entry in read {
            let entry = entry.map_err(|e| SessionError::Io(dir.to_path_buf(), e))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                out.push(stem.to_string());
            }
        }
        out.sort();
        Ok(out)
    }
}

/// `cwd_latest.json`: a map of canonical workspace root to the session id most
/// recently used there. Kept separate from the session files so a `/continue`
/// can resolve "which session did I last use in this directory?" with a single
/// read instead of scanning every session.
fn recent_path(dir: &Path) -> PathBuf {
    dir.join("cwd_latest.json")
}

fn load_recent(dir: &Path) -> Result<std::collections::BTreeMap<String, String>, SessionError> {
    let path = recent_path(dir);
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| SessionError::Parse(path, 1, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(SessionError::Io(path, e)),
    }
}

fn save_recent(
    dir: &Path,
    map: &std::collections::BTreeMap<String, String>,
) -> Result<(), SessionError> {
    let path = recent_path(dir);
    let tmp = dir.join("cwd_latest.json.tmp");
    let text = serde_json::to_string_pretty(map).expect("recent map serialization cannot fail");
    fs::write(&tmp, text).map_err(|e| SessionError::Io(path.clone(), e))?;
    fs::rename(&tmp, &path).map_err(|e| SessionError::Io(path.clone(), e))
}

/// Remember that `session_id` is the most recent session used in `root`.
pub fn record_recent(dir: &Path, root: &Path, session_id: &str) -> Result<(), SessionError> {
    let mut map = load_recent(dir)?;
    map.insert(canonical_root(root), session_id.to_string());
    save_recent(dir, &map)
}

/// The most recent session id recorded for `root`, if any.
pub fn recent_session_id(dir: &Path, root: &Path) -> Result<Option<String>, SessionError> {
    Ok(load_recent(dir)?.get(&canonical_root(root)).cloned())
}

fn read_first_line(path: &Path) -> Result<Option<String>, SessionError> {
    match fs::File::open(path) {
        Ok(f) => {
            let mut line = String::new();
            let mut reader = BufReader::new(f);
            let n = reader
                .read_line(&mut line)
                .map_err(|e| SessionError::Io(path.to_path_buf(), e))?;
            if n == 0 {
                return Ok(None);
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            Ok(Some(trimmed.to_string()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(SessionError::Io(path.to_path_buf(), e)),
    }
}

/// Parse one JSONL line into records.
///
/// 1. Invalid JSON → Err.
/// 2. Object with a known `type` tag → deserialize as `Record` (malformed = Err).
/// 3. Object with an unknown `type` tag → skip.
/// 4. No `type`: legacy envelope, then bare `Message`. Both fail → Err.
fn parse_line(line: &str) -> Result<Vec<Record>, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    if let Some(tag) = value.get("type").and_then(|t| t.as_str()) {
        return match tag {
            "message" | "token_usage" | "session_meta" | "todo_list" => {
                serde_json::from_value(value).map(|r| vec![r])
            }
            _ => Ok(vec![]),
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

fn write_record(file: &mut impl Write, record: &Record) -> std::io::Result<()> {
    let line = serde_json::to_string(record).expect("record serialization cannot fail");
    writeln!(file, "{line}")
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
    use oven_llm::ContentBlock;

    fn tmp() -> tempdir::TempDir {
        tempdir::TempDir::new("oven-session").unwrap()
    }

    fn message_record(timestamp: u64, message: Message) -> Record {
        Record::Message { timestamp, message }
    }

    fn usage_record(timestamp: u64, usage: Usage) -> Record {
        Record::TokenUsage { timestamp, usage }
    }

    #[test]
    fn load_missing_returns_empty() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "missing").unwrap();
        assert!(session.load().unwrap().is_empty());
    }

    #[test]
    fn overwrite_truncates() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").unwrap();
        session
            .append_records(&[
                message_record(1, Message::user_text("a")),
                message_record(2, Message::user_text("b")),
                message_record(3, Message::user_text("c")),
            ])
            .unwrap();
        session
            .overwrite(&[message_record(1, Message::user_text("only"))])
            .unwrap();
        assert_eq!(session.load().unwrap().len(), 1);
    }

    #[test]
    fn records_roundtrip_preserves_usage_and_timestamps() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").unwrap();
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
            .unwrap();

        let loaded = session.load_records().unwrap();
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
        // load() drops the token-usage record.
        assert_eq!(session.load().unwrap().len(), 2);
    }

    #[test]
    fn load_accepts_legacy_bare_message_lines() {
        use std::io::Write;

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").unwrap();
        let mut file = std::fs::File::create(session.path()).unwrap();
        writeln!(
            file,
            "{}",
            serde_json::to_string(&Message::user_text("old")).unwrap()
        )
        .unwrap();
        file.flush().unwrap();

        let loaded = session.load_records().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(
            matches!(&loaded[0], Record::Message { timestamp: 0, message } if message.role == oven_llm::Role::User)
        );
        assert_eq!(session.load().unwrap().len(), 1);
    }

    #[test]
    fn load_accepts_legacy_envelope_lines() {
        use std::io::Write;

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").unwrap();
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

        let loaded = session.load_records().unwrap();
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
        // load() drops the token-usage record.
        assert_eq!(session.load().unwrap().len(), 2);
    }

    #[test]
    fn bad_id_rejected() {
        let tmp = tmp();
        assert!(Session::open(tmp.path(), "../escape").is_err());
        assert!(Session::open(tmp.path(), "a/b").is_err());
        assert!(Session::open(tmp.path(), "").is_err());
    }

    #[test]
    fn session_meta_record_roundtrips_and_load_drops_it() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").unwrap();
        let meta = SessionMeta {
            root: "/ws".into(),
            created_at: 123,
        };
        session
            .append_records(&[
                Record::SessionMeta(meta.clone()),
                message_record(1, Message::user_text("hello")),
            ])
            .unwrap();

        let loaded = session.load_records().unwrap();
        assert!(
            matches!(&loaded[0], Record::SessionMeta(m) if m == &meta),
            "meta must parse from the first line"
        );
        assert_eq!(session.load_meta().unwrap(), Some(meta));
        assert_eq!(session.load().unwrap().len(), 1);
    }

    #[test]
    fn load_meta_missing_or_legacy_returns_none() {
        let tmp = tmp();
        let missing = Session::open(tmp.path(), "missing").unwrap();
        assert_eq!(missing.load_meta().unwrap(), None);

        let legacy = Session::open(tmp.path(), "legacy").unwrap();
        legacy
            .append_records(&[message_record(1, Message::user_text("old"))])
            .unwrap();
        assert_eq!(legacy.load_meta().unwrap(), None);
    }

    #[test]
    fn recent_index_records_latest_session_per_root() {
        let tmp = tmp();
        let root = PathBuf::from("/ws");
        assert_eq!(recent_session_id(tmp.path(), &root).unwrap(), None);

        record_recent(tmp.path(), &root, "s1").unwrap();
        assert_eq!(
            recent_session_id(tmp.path(), &root).unwrap().as_deref(),
            Some("s1")
        );

        record_recent(tmp.path(), &root, "s2").unwrap();
        assert_eq!(
            recent_session_id(tmp.path(), &root).unwrap().as_deref(),
            Some("s2"),
            "latest recording wins"
        );

        assert_eq!(
            recent_session_id(tmp.path(), Path::new("/other")).unwrap(),
            None
        );

        assert!(tmp.path().join("cwd_latest.json").exists());
        assert!(!tmp.path().join("cwd_latest.json.tmp").exists());
    }

    #[test]
    fn unknown_type_is_skipped_and_messages_still_load() {
        use std::io::Write;

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").unwrap();
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

        let loaded = session.load_records().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(
            matches!(&loaded[0], Record::Message { message, .. } if message.role == oven_llm::Role::User)
        );
        assert_eq!(session.load().unwrap().len(), 1);
    }

    #[test]
    fn legacy_envelope_still_loads_after_type_skip() {
        use std::io::Write;

        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").unwrap();
        let mut file = std::fs::File::create(session.path()).unwrap();
        writeln!(file, r#"{{"type":"future_widget","payload":1}}"#).unwrap();
        writeln!(
            file,
            r#"{{"message":{},"usage":{{"input_tokens":4,"output_tokens":1}}}}"#,
            serde_json::to_string(&Message::user_text("old")).unwrap()
        )
        .unwrap();
        file.flush().unwrap();

        let loaded = session.load_records().unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(
            matches!(&loaded[0], Record::Message { timestamp: 0, message } if message.role == oven_llm::Role::User)
        );
        assert!(matches!(
            &loaded[1],
            Record::TokenUsage { usage, .. } if usage.input_tokens == 4
        ));
    }

    #[test]
    fn load_drops_todo_list_records() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").unwrap();
        session
            .append_records(&[
                message_record(1, Message::user_text("hello")),
                Record::TodoList {
                    timestamp: 2,
                    items: vec![],
                },
                message_record(3, Message::assistant(vec![ContentBlock::text("hi")])),
            ])
            .unwrap();

        let loaded = session.load_records().unwrap();
        assert_eq!(loaded.len(), 3);
        assert!(matches!(&loaded[1], Record::TodoList { items, .. } if items.is_empty()));
        assert_eq!(session.load().unwrap().len(), 2);
    }

    #[test]
    fn known_type_malformed_is_error() {
        let err = parse_line(r#"{"type":"todo_list"}"#).unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}

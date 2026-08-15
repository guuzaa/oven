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

use oven_agent::Record;
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
                    Record::TokenUsage { .. } => None,
                })
                .collect()
        })
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

/// Parse one JSONL line into records. Tries the current `Record` format, the
/// legacy envelope (which can expand into a message plus a `TokenUsage`
/// record), then a bare `Message`.
fn parse_line(line: &str) -> Result<Vec<Record>, serde_json::Error> {
    match serde_json::from_str::<Record>(line) {
        Ok(record) => Ok(vec![record]),
        Err(record_err) => match serde_json::from_str::<RecordLine>(line) {
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
            Err(_) => match serde_json::from_str::<Message>(line) {
                Ok(message) => Ok(vec![Record::Message {
                    timestamp: 0,
                    message,
                }]),
                Err(_) => Err(record_err),
            },
        },
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
}

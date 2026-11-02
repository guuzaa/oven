//! The persisted shape of a conversation.
//!
//! One [`Record`] per JSONL line, so a session file appends without rewriting
//! what came before. The format is part of the protocol: a frontend that
//! reloads a session reads the same records the runtime wrote.

use oven_llm::{Message, Usage};
use serde::{Deserialize, Serialize};

use crate::TodoItem;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    Message {
        /// Unix milliseconds when the message was created.
        timestamp: u64,
        #[serde(flatten)]
        message: Message,
    },
    TokenUsage {
        /// Unix milliseconds; the timestamp of the assistant message this
        /// usage belongs to.
        timestamp: u64,
        #[serde(flatten)]
        usage: Usage,
    },
    /// Thinking span for the preceding assistant message. `timestamp` is when
    /// thinking started; `duration_ms` is wall time until thinking ended.
    Thinking { timestamp: u64, duration_ms: u64 },
    /// Session-level metadata, written as the first line of a session file.
    SessionMeta(SessionMeta),
    TodoList {
        timestamp: u64,
        items: Vec<TodoItem>,
    },
}

/// The session format this build writes.
///
/// A file records its format and keeps it: a resumed session is written in
/// the format of its file.
///
/// The version moves on only when an older build would misread a file (a
/// field renamed, removed or made required, a record whose meaning changed).
/// A new optional field or record type does not move it: older builds skip
/// what they do not know. A new version needs an upgrade step for reading
/// and a case in [`Record::into_version`] for writing.
///
/// Format 2 is the first to record its version (1 means none was recorded)
/// and added the working directory to the meta.
pub const SESSION_FORMAT_VERSION: u32 = 2;

/// The format of a session file written before the version was recorded.
///
/// The field arrived with format 1, so a file without it is in format 1. When
/// the format moves on this stays at 1: an old file then reads as 1 and a new
/// one as the version it was written in, which is what tells them apart.
pub const SESSION_FORMAT_WITHOUT_VERSION: u32 = 1;

/// The session format that records the working directory.
///
/// Before it, a session file says where it works in no way at all.
const WORKING_DIR_FORMAT: u32 = 2;

/// Where and when a session was created. Written as the first JSONL record so
/// the workspace root survives a resume.
///
/// The first record names the file's format; a file that does not start
/// with a meta is in [`SESSION_FORMAT_WITHOUT_VERSION`]. A later meta that
/// names an unreadable format still refuses the file.
///
/// Readers take `format_version` from the raw JSON before reading the meta,
/// so the `session_meta` tag and the integer `format_version` field keep
/// their name and type in every format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMeta {
    pub root: String,
    /// Unix milliseconds when the session first got content.
    pub created_at: u64,
    /// The session format this file was written in.
    #[serde(default = "session_format_without_version")]
    pub format_version: u32,
    /// The directory the session was started in.
    ///
    /// A file written before the session recorded it has none: there is
    /// nothing to compare it against, so it is ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

impl SessionMeta {
    /// Whether this build can read a file written in this format.
    ///
    /// A file written by a newer oven may hold records this build does not
    /// know, so it is refused rather than half-understood.
    pub fn is_readable_by_this_build(&self) -> bool {
        Self::format_is_readable_by_this_build(self.format_version)
    }

    /// Whether this build reads `version`: from
    /// [`SESSION_FORMAT_WITHOUT_VERSION`] to [`SESSION_FORMAT_VERSION`].
    pub fn format_is_readable_by_this_build(version: u32) -> bool {
        (SESSION_FORMAT_WITHOUT_VERSION..=SESSION_FORMAT_VERSION).contains(&version)
    }

    /// This meta as a file in `version` writes it.
    ///
    /// A file keeps the format it was written in: a session in an older
    /// format is written in that format, so the records in it mean what they
    /// meant then. What that format does not know, it does not get.
    pub fn for_version(&self, version: u32) -> SessionMeta {
        let mut meta = self.clone();
        meta.format_version = version;
        if version < WORKING_DIR_FORMAT {
            meta.working_dir = None;
        }
        meta
    }
}

impl Record {
    /// The record as a file in `version` writes it.
    pub fn into_version(self, version: u32) -> Record {
        match self {
            Record::SessionMeta(meta) => Record::SessionMeta(meta.for_version(version)),
            other => other,
        }
    }
}

/// The format version of a file that records none.
fn session_format_without_version() -> u32 {
    SESSION_FORMAT_WITHOUT_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_survives_the_wire() {
        let record = Record::Thinking {
            timestamp: 1,
            duration_ms: 2,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(json, r#"{"type":"thinking","timestamp":1,"duration_ms":2}"#);
    }

    #[test]
    fn the_format_version_survives_the_wire() {
        let meta = SessionMeta {
            root: "/ws".into(),
            created_at: 1,
            format_version: SESSION_FORMAT_VERSION,
            working_dir: None,
        };
        let json = serde_json::to_string(&Record::SessionMeta(meta)).unwrap();
        assert!(
            json.contains(&format!(r#""format_version":{SESSION_FORMAT_VERSION}"#)),
            "{json}"
        );
    }

    #[test]
    fn a_meta_without_a_version_reads_as_the_format_that_introduced_it() {
        let meta: SessionMeta = serde_json::from_str(r#"{"root":"/ws","created_at":1}"#).unwrap();
        assert_eq!(meta.format_version, SESSION_FORMAT_WITHOUT_VERSION);
    }

    #[test]
    fn a_meta_without_a_working_dir_records_none() {
        let meta: SessionMeta = serde_json::from_str(r#"{"root":"/ws","created_at":1}"#).unwrap();
        assert_eq!(meta.working_dir, None);
    }

    #[test]
    fn the_working_dir_survives_the_wire() {
        let meta = SessionMeta {
            root: "/ws".into(),
            created_at: 1,
            format_version: SESSION_FORMAT_VERSION,
            working_dir: Some("/ws".into()),
        };
        let json = serde_json::to_string(&Record::SessionMeta(meta)).unwrap();
        assert!(json.contains(r#""working_dir":"/ws""#), "{json}");
    }

    #[test]
    fn a_file_this_build_cannot_read_is_not_readable_by_it() {
        let meta = SessionMeta {
            root: "/ws".into(),
            created_at: 1,
            format_version: SESSION_FORMAT_VERSION + 1,
            working_dir: None,
        };
        assert!(!meta.is_readable_by_this_build());
    }

    #[test]
    fn a_format_below_the_first_is_not_readable_by_this_build() {
        let meta = SessionMeta {
            root: "/ws".into(),
            created_at: 1,
            format_version: SESSION_FORMAT_WITHOUT_VERSION - 1,
            working_dir: None,
        };
        assert!(!meta.is_readable_by_this_build());
    }

    #[test]
    fn every_format_from_the_first_to_this_one_is_readable_by_this_build() {
        for version in SESSION_FORMAT_WITHOUT_VERSION..=SESSION_FORMAT_VERSION {
            assert!(
                SessionMeta::format_is_readable_by_this_build(version),
                "v{version}"
            );
        }
    }

    #[test]
    fn a_meta_written_in_an_older_format_drops_what_it_does_not_know() {
        let meta = SessionMeta {
            root: "/ws".into(),
            created_at: 1,
            format_version: SESSION_FORMAT_VERSION,
            working_dir: Some("/ws".into()),
        };

        let older = meta.for_version(SESSION_FORMAT_WITHOUT_VERSION);
        assert_eq!(older.format_version, SESSION_FORMAT_WITHOUT_VERSION);
        assert_eq!(older.working_dir, None);
        assert_eq!(older.root, "/ws");
    }

    #[test]
    fn a_meta_written_in_its_own_format_keeps_everything() {
        let meta = SessionMeta {
            root: "/ws".into(),
            created_at: 1,
            format_version: SESSION_FORMAT_VERSION,
            working_dir: Some("/ws".into()),
        };

        assert_eq!(meta.for_version(SESSION_FORMAT_VERSION), meta);
    }

    #[test]
    fn a_record_written_in_a_version_keeps_the_rest_of_its_shape() {
        let record = Record::Thinking {
            timestamp: 1,
            duration_ms: 2,
        };
        let json =
            serde_json::to_string(&record.into_version(SESSION_FORMAT_WITHOUT_VERSION)).unwrap();
        assert_eq!(json, r#"{"type":"thinking","timestamp":1,"duration_ms":2}"#);
    }
}

//! The inbox: one project's raw notes, appended during sessions and read by
//! tidy. Machine data, so a JSONL log rather than a directory of files.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tracing::warn;

use crate::error::MemoryError;
use crate::model::{MAX_NOTE, MemoryKind};

/// Why the model believes what it noted. Text it only read is not a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    Verified,
    UserSaid,
}

impl Evidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::UserSaid => "user_said",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        [Self::Verified, Self::UserSaid]
            .into_iter()
            .find(|evidence| evidence.as_str() == raw)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub kind: MemoryKind,
    pub evidence: Evidence,
    pub text: String,
    /// The `scope/id` of a memory this note corrects or confirms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
}

/// The notes an inbox held when it was read. `ends[i]` is the byte offset
/// just past note `i`'s line, and `len` the size of the whole file, so
/// consuming a prefix leaves whatever was appended afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InboxSnapshot {
    pub notes: Vec<Note>,
    pub ends: Vec<usize>,
    pub len: usize,
}

/// The leading notes a run takes, and the bytes to consume for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub notes: Vec<Note>,
    /// Notes after this batch, which stay in the inbox.
    pub left: usize,
    pub consumed: usize,
}

impl InboxSnapshot {
    /// The leading notes whose text fits in `max_chars`. The first note is
    /// always taken, so an over-long note cannot block the inbox forever.
    pub fn batch(&self, max_chars: usize) -> Batch {
        let mut taken = 0;
        let mut chars = 0;
        for note in &self.notes {
            let size = note.text.chars().count();
            if taken > 0 && chars + size > max_chars {
                break;
            }
            chars += size;
            taken += 1;
        }
        let consumed = match taken {
            0 => self.len,
            count if count == self.notes.len() => self.len,
            count => self.ends[count - 1],
        };
        Batch {
            notes: self.notes[..taken].to_vec(),
            left: self.notes.len() - taken,
            consumed,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Inbox {
    path: PathBuf,
}

impl Inbox {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one note and returns how many are waiting, this one included.
    pub async fn append(&self, mut note: Note) -> Result<usize, MemoryError> {
        note.text = note.text.trim().to_owned();
        if note.text.is_empty() {
            return Err(MemoryError::EmptyNote);
        }
        if note.text.chars().count() > MAX_NOTE {
            return Err(MemoryError::NoteTooLong);
        }
        let mut line = serde_json::to_string(&note).map_err(|err| MemoryError::Render {
            message: err.to_string(),
        })?;
        line.push('\n');
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).await?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        Ok(self.count().await)
    }

    pub async fn read(&self) -> Result<InboxSnapshot, MemoryError> {
        let raw = match fs::read_to_string(&self.path).await {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(InboxSnapshot::default());
            }
            Err(err) => return Err(err.into()),
        };
        let mut notes = Vec::new();
        let mut ends = Vec::new();
        let mut offset = 0;
        for piece in raw.split_inclusive('\n') {
            offset += piece.len();
            let line = piece.trim_end_matches(['\n', '\r']);
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(note) => {
                    notes.push(note);
                    ends.push(offset);
                }
                Err(err) => {
                    warn!(path = %self.path.display(), error = %err, "skipping unparsable inbox note");
                }
            }
        }
        Ok(InboxSnapshot {
            notes,
            ends,
            len: raw.len(),
        })
    }

    pub async fn count(&self) -> usize {
        self.read()
            .await
            .map(|snapshot| snapshot.notes.len())
            .unwrap_or_default()
    }

    /// Drops the first `len` bytes: the notes a snapshot of that length
    /// held. Lines appended after the snapshot stay.
    pub async fn consume(&self, len: usize) -> Result<(), MemoryError> {
        let raw = match fs::read(&self.path).await {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        let rest = raw.get(len..).unwrap_or_default();
        if rest.is_empty() {
            return match fs::remove_file(&self.path).await {
                Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err.into()),
                _ => Ok(()),
            };
        }
        oven_host::write_atomic(&self.path, rest.to_vec()).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Evidence, Inbox, Note};
    use crate::error::{EMPTY_NOTE, MemoryError, NOTE_TOO_LONG};
    use crate::model::{MAX_NOTE, MemoryKind};

    fn note(text: &str) -> Note {
        Note {
            at: "2026-11-16T10:00:00Z".into(),
            session: Some("01J".into()),
            kind: MemoryKind::Preference,
            evidence: Evidence::UserSaid,
            text: text.into(),
            about: None,
        }
    }

    fn inbox(tmp: &tempdir::TempDir) -> Inbox {
        Inbox::new(tmp.path().join("project").join("inbox.jsonl"))
    }

    #[tokio::test]
    async fn append_and_read_round_trip() {
        let tmp = tempdir::TempDir::new("inbox-round-trip").unwrap();
        let inbox = inbox(&tmp);
        assert_eq!(inbox.read().await.unwrap().notes, vec![]);
        assert_eq!(inbox.append(note(" first ")).await.unwrap(), 1);
        let mut second = note("second");
        second.about = Some("project/x".into());
        assert_eq!(inbox.append(second.clone()).await.unwrap(), 2);
        let snapshot = inbox.read().await.unwrap();
        assert_eq!(snapshot.notes, vec![note("first"), second]);
    }

    #[tokio::test]
    async fn rejects_empty_and_long_notes() {
        let tmp = tempdir::TempDir::new("inbox-limits").unwrap();
        let inbox = inbox(&tmp);
        let err = inbox.append(note("  ")).await.unwrap_err();
        assert_eq!(err, MemoryError::EmptyNote);
        assert_eq!(err.to_string(), EMPTY_NOTE);
        let err = inbox
            .append(note(&"n".repeat(MAX_NOTE + 1)))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), format!("{NOTE_TOO_LONG} ({MAX_NOTE})"));
        assert!(!inbox.path().exists());
    }

    #[tokio::test]
    async fn an_unparsable_line_is_skipped_not_fatal() {
        let tmp = tempdir::TempDir::new("inbox-bad-line").unwrap();
        let inbox = inbox(&tmp);
        inbox.append(note("kept")).await.unwrap();
        let mut raw = std::fs::read_to_string(inbox.path()).unwrap();
        raw.push_str("not json\n");
        std::fs::write(inbox.path(), raw).unwrap();
        assert_eq!(inbox.read().await.unwrap().notes, vec![note("kept")]);
    }

    #[tokio::test]
    async fn a_batch_takes_leading_notes_within_its_budget() {
        let tmp = tempdir::TempDir::new("inbox-batch").unwrap();
        let inbox = inbox(&tmp);
        for text in ["aaaa", "bb", "cccccc"] {
            inbox.append(note(text)).await.unwrap();
        }
        let snapshot = inbox.read().await.unwrap();
        let batch = snapshot.batch(6);
        assert_eq!(batch.notes, vec![note("aaaa"), note("bb")]);
        assert_eq!(batch.left, 1);
        inbox.consume(batch.consumed).await.unwrap();
        assert_eq!(inbox.read().await.unwrap().notes, vec![note("cccccc")]);
    }

    #[tokio::test]
    async fn a_batch_always_takes_its_first_note() {
        let tmp = tempdir::TempDir::new("inbox-batch-first").unwrap();
        let inbox = inbox(&tmp);
        inbox.append(note("a long note")).await.unwrap();
        inbox.append(note("next")).await.unwrap();
        let batch = inbox.read().await.unwrap().batch(3);
        assert_eq!(batch.notes, vec![note("a long note")]);
        assert_eq!(batch.left, 1);
    }

    #[tokio::test]
    async fn a_batch_of_everything_consumes_the_whole_file() {
        let tmp = tempdir::TempDir::new("inbox-batch-all").unwrap();
        let inbox = inbox(&tmp);
        inbox.append(note("only")).await.unwrap();
        let snapshot = inbox.read().await.unwrap();
        let batch = snapshot.batch(1000);
        assert_eq!(batch.left, 0);
        assert_eq!(batch.consumed, snapshot.len);
    }

    #[tokio::test]
    async fn consume_keeps_what_was_appended_after_the_read() {
        let tmp = tempdir::TempDir::new("inbox-consume").unwrap();
        let inbox = inbox(&tmp);
        inbox.append(note("read")).await.unwrap();
        let snapshot = inbox.read().await.unwrap();
        inbox.append(note("late")).await.unwrap();
        inbox.consume(snapshot.len).await.unwrap();
        assert_eq!(inbox.read().await.unwrap().notes, vec![note("late")]);
        let snapshot = inbox.read().await.unwrap();
        inbox.consume(snapshot.len).await.unwrap();
        assert!(!inbox.path().exists());
        inbox.consume(0).await.unwrap();
    }
}

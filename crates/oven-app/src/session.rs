//! Conversation persistence as JSONL.
//!
//! Each session is one file under `<data_dir>/oven/sessions/<id>.jsonl`. The
//! file holds one `Message` per line, appended as the conversation progresses
//! so a crash never loses already-committed turns beyond the last flushed
//! line.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use oven_llm::Message;
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
/// `~/.local/share/oven/sessions/` on Linux, `~/Library/Application
/// Support/oven/sessions/` on macOS).
pub fn default_sessions_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("oven").join("sessions"))
}

#[derive(Debug, Clone)]
pub struct Session {
    id: String,
    path: PathBuf,
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

    /// Read all messages from disk. Returns an empty Vec if the file does not
    /// yet exist.
    pub fn load(&self) -> Result<Vec<Message>, SessionError> {
        match fs::File::open(&self.path) {
            Ok(f) => {
                let reader = BufReader::new(f);
                let mut out = Vec::new();
                for (i, line) in reader.lines().enumerate() {
                    let line = line.map_err(|e| SessionError::Io(self.path.clone(), e))?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    let msg: Message = serde_json::from_str(&line)
                        .map_err(|e| SessionError::Parse(self.path.clone(), i + 1, e))?;
                    out.push(msg);
                }
                Ok(out)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(SessionError::Io(self.path.clone(), e)),
        }
    }

    /// Append a single message as one JSONL line.
    pub fn append(&self, message: &Message) -> Result<(), SessionError> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| SessionError::Io(self.path.clone(), e))?;
        let line = serde_json::to_string(message)
            .map_err(|e| SessionError::Parse(self.path.clone(), 0, e))?;
        writeln!(file, "{line}").map_err(|e| SessionError::Io(self.path.clone(), e))?;
        file.flush()
            .map_err(|e| SessionError::Io(self.path.clone(), e))?;
        Ok(())
    }

    /// Append many messages in one open/flush cycle.
    pub fn append_all(&self, messages: &[Message]) -> Result<(), SessionError> {
        if messages.is_empty() {
            return Ok(());
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| SessionError::Io(self.path.clone(), e))?;
        for m in messages {
            let line = serde_json::to_string(m)
                .map_err(|e| SessionError::Parse(self.path.clone(), 0, e))?;
            writeln!(file, "{line}").map_err(|e| SessionError::Io(self.path.clone(), e))?;
        }
        file.flush()
            .map_err(|e| SessionError::Io(self.path.clone(), e))?;
        Ok(())
    }

    /// Replace the entire session file with `messages`. Useful when trimming
    /// context in memory and persisting the trimmed version.
    pub fn overwrite(&self, messages: &[Message]) -> Result<(), SessionError> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&self.path)
            .map_err(|e| SessionError::Io(self.path.clone(), e))?;
        for m in messages {
            let line = serde_json::to_string(m)
                .map_err(|e| SessionError::Parse(self.path.clone(), 0, e))?;
            writeln!(file, "{line}").map_err(|e| SessionError::Io(self.path.clone(), e))?;
        }
        file.flush()
            .map_err(|e| SessionError::Io(self.path.clone(), e))?;
        Ok(())
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

    #[test]
    fn append_then_load_roundtrip() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s1").unwrap();
        session.append(&Message::user_text("hello")).unwrap();
        session
            .append(&Message::assistant(vec![ContentBlock::text("hi")]))
            .unwrap();
        let loaded = session.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(
            matches!(loaded[0].content.first(), Some(ContentBlock::Text { text }) if text == "hello")
        );
        assert_eq!(loaded[0].role, oven_llm::Role::User);
        assert_eq!(loaded[1].role, oven_llm::Role::Assistant);
    }

    #[test]
    fn load_missing_returns_empty() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "missing").unwrap();
        assert!(session.load().unwrap().is_empty());
    }

    #[test]
    fn list_returns_sorted_ids() {
        let tmp = tmp();
        Session::open(tmp.path(), "b")
            .unwrap()
            .append(&Message::user_text("x"))
            .unwrap();
        Session::open(tmp.path(), "a")
            .unwrap()
            .append(&Message::user_text("x"))
            .unwrap();
        let ids = Session::list(tmp.path()).unwrap();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn overwrite_truncates() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "s").unwrap();
        session
            .append_all(&[
                Message::user_text("a"),
                Message::user_text("b"),
                Message::user_text("c"),
            ])
            .unwrap();
        session.overwrite(&[Message::user_text("only")]).unwrap();
        assert_eq!(session.load().unwrap().len(), 1);
    }

    #[test]
    fn bad_id_rejected() {
        let tmp = tmp();
        assert!(Session::open(tmp.path(), "../escape").is_err());
        assert!(Session::open(tmp.path(), "a/b").is_err());
        assert!(Session::open(tmp.path(), "").is_err());
    }

    #[test]
    fn delete_is_idempotent() {
        let tmp = tmp();
        let session = Session::open(tmp.path(), "x").unwrap();
        session.append(&Message::user_text("hi")).unwrap();
        session.delete().unwrap();
        session.delete().unwrap();
        assert!(session.load().unwrap().is_empty());
    }
}

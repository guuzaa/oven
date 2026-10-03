use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::MemoryError;

pub const MAX_ID: usize = 64;
pub const MAX_DESCRIPTION: usize = 120;
pub const MAX_BODY: usize = 4000;
pub const MAX_CATALOG_CHARS: usize = 8000;

const ID_DASH: char = '-';

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemoryId(String);

impl MemoryId {
    pub fn new(raw: &str) -> Result<Self, MemoryError> {
        if raw.is_empty() {
            return Err(MemoryError::EmptyId);
        }
        if !raw.chars().all(is_id_char) {
            return Err(MemoryError::InvalidId);
        }
        if raw.starts_with(ID_DASH) {
            return Err(MemoryError::LeadingDash);
        }
        if raw.chars().count() > MAX_ID {
            return Err(MemoryError::IdTooLong);
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn is_id_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == ID_DASH
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    Fact,
    Preference,
}

impl MemoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Preference => "preference",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryScope {
    Workspace,
    User,
}

impl MemoryScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::User => "user",
        }
    }
}

impl fmt::Display for MemoryScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    pub id: MemoryId,
    pub kind: MemoryKind,
    pub description: String,
    pub body: String,
    pub scope: MemoryScope,
    pub source: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{MAX_ID, MemoryId};
    use crate::error::{EMPTY_ID, ID_TOO_LONG, INVALID_ID, LEADING_DASH, MemoryError};

    #[test]
    fn valid_slug_ok() {
        let id = MemoryId::new("proxy-requires-http2").unwrap();
        assert_eq!(id.as_str(), "proxy-requires-http2");
        assert!(MemoryId::new(&"a".repeat(MAX_ID)).is_ok());
    }

    #[test]
    fn rejects_invalid_ids() {
        assert_eq!(MemoryId::new("../x"), Err(MemoryError::InvalidId));
        assert_eq!(MemoryId::new("../x").unwrap_err().to_string(), INVALID_ID);
        assert_eq!(MemoryId::new("a/b"), Err(MemoryError::InvalidId));
        assert_eq!(MemoryId::new("A"), Err(MemoryError::InvalidId));
        assert_eq!(MemoryId::new(""), Err(MemoryError::EmptyId));
        assert_eq!(MemoryId::new("").unwrap_err().to_string(), EMPTY_ID);
        assert_eq!(MemoryId::new("-leading"), Err(MemoryError::LeadingDash));
        assert_eq!(
            MemoryId::new("-leading").unwrap_err().to_string(),
            LEADING_DASH
        );
        let too_long = "a".repeat(MAX_ID + 1);
        assert_eq!(MemoryId::new(&too_long), Err(MemoryError::IdTooLong));
        assert_eq!(
            MemoryId::new(&too_long).unwrap_err().to_string(),
            format!("{ID_TOO_LONG} ({MAX_ID})")
        );
    }
}

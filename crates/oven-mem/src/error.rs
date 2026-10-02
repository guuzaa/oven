use thiserror::Error;

use crate::model::MAX_ID;

pub const EMPTY_ID: &str = "memory id is empty";
pub const LEADING_DASH: &str = "memory id must not start with '-'";
pub const ID_TOO_LONG: &str = "memory id exceeds MAX_ID";
pub const INVALID_ID: &str = "memory id must match [a-z0-9-]+";
pub const MISSING_FRONTMATTER: &str = "memory file is missing frontmatter";
pub const MISSING_KIND: &str = "memory frontmatter is missing kind";
pub const UNKNOWN_KIND: &str = "unknown memory kind";
pub const EXPECTED_KINDS: &str = "fact or preference";
pub const MISSING_DESCRIPTION: &str = "memory description is missing or empty";
pub const INVALID_FRONTMATTER: &str = "memory frontmatter is not valid yaml";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MemoryError {
    #[error("{EMPTY_ID}")]
    EmptyId,
    #[error("{LEADING_DASH}")]
    LeadingDash,
    #[error("{ID_TOO_LONG} ({MAX_ID})")]
    IdTooLong,
    #[error("{INVALID_ID}")]
    InvalidId,
    #[error("{MISSING_FRONTMATTER}")]
    MissingFrontmatter,
    #[error("{MISSING_KIND}")]
    MissingKind,
    #[error("{UNKNOWN_KIND} '{kind}'; expected {EXPECTED_KINDS}")]
    UnknownKind { kind: String },
    #[error("{MISSING_DESCRIPTION}")]
    MissingDescription,
    #[error("{INVALID_FRONTMATTER}")]
    InvalidFrontmatter,
}

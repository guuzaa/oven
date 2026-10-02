use thiserror::Error;

use crate::model::MAX_ID;

pub const EMPTY_ID: &str = "memory id is empty";
pub const LEADING_DASH: &str = "memory id must not start with '-'";
pub const ID_TOO_LONG: &str = "memory id exceeds MAX_ID";
pub const INVALID_ID: &str = "memory id must match [a-z0-9-]+";

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
}

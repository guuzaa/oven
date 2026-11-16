use thiserror::Error;

use crate::model::{
    MAX_ANCHOR, MAX_ANCHORS, MAX_BODY, MAX_DESCRIPTION, MAX_ID, MAX_NOTE, MAX_WHY, MemoryId,
    MemoryScope,
};

pub const EMPTY_ID: &str = "memory id is empty";
pub const LEADING_DASH: &str = "memory id must not start with '-'";
pub const ID_TOO_LONG: &str = "memory id exceeds MAX_ID";
pub const INVALID_ID: &str = "memory id must match [a-z0-9-]+";
pub const RESERVED_ID: &str = "memory id is reserved by OKF";
pub const MISSING_FRONTMATTER: &str = "memory file is missing frontmatter";
pub const MISSING_TYPE: &str = "memory frontmatter is missing type";
pub const MISSING_DESCRIPTION: &str = "memory description is missing or empty";
pub const INVALID_FRONTMATTER: &str = "memory frontmatter is not valid yaml";
pub const NOT_FOUND: &str = "memory not found";
pub const ALREADY_EXISTS: &str = "memory already exists";
pub const LAYER_UNAVAILABLE: &str = "memory layer is unavailable without a home directory";
pub const SHARED_READ_ONLY: &str = "the shared layer is written by people, not by the model";
pub const DESCRIPTION_TOO_LONG: &str = "memory description exceeds MAX_DESCRIPTION";
pub const WHY_TOO_LONG: &str = "memory why exceeds MAX_WHY";
pub const BODY_TOO_LONG: &str = "memory body exceeds MAX_BODY";
pub const PREFERENCE_BODY: &str =
    "a preference has no body; put the rule in description and the reason in why";
pub const TOO_MANY_ANCHORS: &str = "memory anchors exceed MAX_ANCHORS";
pub const INVALID_ANCHOR: &str = "memory anchor must be a workspace-relative path without '..'";
pub const LAYER_FULL: &str = "memory layer budget is full; run /memory tidy";
pub const EMPTY_NOTE: &str = "memory note is empty";
pub const NOTE_TOO_LONG: &str = "memory note exceeds MAX_NOTE";
pub const RENDER_FAILED: &str = "memory frontmatter could not be rendered";
pub const INVALID_PLAN: &str = "tidy reply is not a valid plan";

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
    #[error("{RESERVED_ID}: {id}")]
    ReservedId { id: String },
    #[error("{MISSING_FRONTMATTER}")]
    MissingFrontmatter,
    #[error("{MISSING_TYPE}")]
    MissingType,
    #[error("{MISSING_DESCRIPTION}")]
    MissingDescription,
    #[error("{INVALID_FRONTMATTER}")]
    InvalidFrontmatter,
    #[error("{NOT_FOUND}: {scope}/{id}")]
    NotFound { scope: MemoryScope, id: MemoryId },
    #[error("{ALREADY_EXISTS}: {scope}/{id}")]
    AlreadyExists { scope: MemoryScope, id: MemoryId },
    #[error("{LAYER_UNAVAILABLE}: {scope}")]
    LayerUnavailable { scope: MemoryScope },
    #[error("{SHARED_READ_ONLY}")]
    SharedReadOnly,
    #[error("{DESCRIPTION_TOO_LONG} ({MAX_DESCRIPTION})")]
    DescriptionTooLong,
    #[error("{WHY_TOO_LONG} ({MAX_WHY})")]
    WhyTooLong,
    #[error("{BODY_TOO_LONG} ({MAX_BODY})")]
    BodyTooLong,
    #[error("{PREFERENCE_BODY}")]
    PreferenceBody,
    #[error("{TOO_MANY_ANCHORS} ({MAX_ANCHORS})")]
    TooManyAnchors,
    #[error("{INVALID_ANCHOR} (at most {MAX_ANCHOR} chars): {anchor}")]
    InvalidAnchor { anchor: String },
    #[error("{LAYER_FULL} ({scope}: {budget} chars)")]
    LayerFull { scope: MemoryScope, budget: usize },
    #[error("{EMPTY_NOTE}")]
    EmptyNote,
    #[error("{NOTE_TOO_LONG} ({MAX_NOTE})")]
    NoteTooLong,
    #[error("{RENDER_FAILED}: {message}")]
    Render { message: String },
    #[error("{INVALID_PLAN}: {message}")]
    InvalidPlan { message: String },
    #[error("{message}")]
    Io { message: String },
}

impl From<std::io::Error> for MemoryError {
    fn from(err: std::io::Error) -> Self {
        Self::Io {
            message: err.to_string(),
        }
    }
}

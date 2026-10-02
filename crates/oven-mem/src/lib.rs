mod error;
mod model;

pub use error::MemoryError;
pub use model::{
    MAX_BODY, MAX_CATALOG_CHARS, MAX_DESCRIPTION, MAX_ID, Memory, MemoryId, MemoryKind, MemoryScope,
};

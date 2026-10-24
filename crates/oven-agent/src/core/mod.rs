//! The nouns `oven-agent` reasons about, and the pure rules between them.
//!
//! Nothing here performs I/O or talks to a provider. The nouns a frontend
//! can see live in `oven-proto`; what stays here is the machinery that turns
//! them into turns.

pub mod history;
pub mod interaction;
pub mod models;
pub mod prompt_template;
pub mod selection;
pub mod subagent;
pub mod tool;
pub mod turn;
pub mod view;

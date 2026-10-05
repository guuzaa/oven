//! The nouns `oven-agent` reasons about, and the pure rules between them.
//!
//! Nothing here performs I/O or talks to a provider.

pub mod error;
pub mod event;
pub mod history;
pub mod identity;
pub mod interaction;
pub mod matching;
pub mod mode;
pub mod models;
pub mod prompt_template;
pub mod selection;
pub mod sink;
pub mod subagent;
pub mod todo;
pub mod turn;
pub mod view;

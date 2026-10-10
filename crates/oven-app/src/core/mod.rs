//! The app's nouns and their pure rules: configuration, published state,
//! events, session persistence and provider construction.

pub mod complete;
pub mod config;
pub mod error;
pub mod event;
pub mod input;
pub mod mention;
pub mod provider;
pub mod session;
mod session_lock;
pub mod state;

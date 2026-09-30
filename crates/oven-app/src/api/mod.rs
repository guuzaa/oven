//! The facade a frontend talks to, and the builder that wires every lower
//! layer together.

pub mod app;
pub mod builder;
pub mod input;

pub use app::App;
pub use builder::AppBuilder;

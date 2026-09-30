//! The terminal presentation layer: it renders the app's state and events,
//! owns the keyboard and the mouse, and sends commands back to `oven-app`.
//! It makes no business decisions.
//!
//! Layers, top to bottom; a layer reaches only into the ones below it:
//!
//! 1. `cli` — the `oven` binary: flags, and which mode they select
//! 2. `runtime` — the `Ui` actor: the event loop, the projection of app
//!    events onto the screen, and the keys that reach the widgets
//! 3. `widgets` — one component per screen region
//! 4. `core` — the vocabulary the widgets share and the pure rules between
//!    them: the component contract, the themes, the screen geometry, the
//!    keys, the composer hints, the `Esc` decision
//! 5. `platform` — the operating system the screens draw on: the terminal's
//!    raw-mode lifecycle, and the clipboard

mod cli;
mod core;
mod platform;
mod runtime;
mod widgets;

pub use cli::Cli;

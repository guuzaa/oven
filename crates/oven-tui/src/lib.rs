//! The terminal presentation layer: it renders the app's state and events,
//! owns the keyboard and the mouse, and sends commands back to `oven-app`.
//! It makes no business decisions.
//!
//! Layers, top to bottom; a layer reaches only into the ones below it:
//!
//! 1. `cli` — the `oven` binary: flags, subcommands, and which mode they select
//! 2. `commands` — the subcommands that run instead of a session: they ask the
//!    questions `oven-app` cannot, and reach config only through it
//! 3. `runtime` — the `Ui` actor: the event loop, the projection of app
//!    events onto the screen, and the keys that reach the widgets
//! 4. `widgets` — one component per screen region
//! 5. `core` — the vocabulary the widgets share and the pure rules between
//!    them: the component contract, the themes, the screen geometry, the
//!    keys, the composer hints, the `Esc` decision
//! 6. `platform` — the operating system the screens draw on: the terminal's
//!    raw-mode lifecycle, and the clipboard

mod cli;
mod commands;
mod core;
mod platform;
mod runtime;
mod widgets;

pub use cli::Cli;

//! What an agent can call: the `Tool` protocol and its built-ins, the skills
//! that contribute guidance, and the protocol for handing work to another
//! agent.
//!
//! A layer reaches down into `core` for the nouns it handles; it never reaches
//! up into `runtime`.

pub mod skills;
pub mod tools;

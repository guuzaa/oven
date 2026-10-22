//! The names the protocol identifies things by.
//!
//! Every id is a counter, so a frontend can order what it receives without
//! asking the app layer. They are plain numbers on the wire; nothing here
//! reaches for a clock or a random source.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

macro_rules! counter_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub u64);

        impl $name {
            pub fn next() -> Self {
                static NEXT: AtomicU64 = AtomicU64::new(1);
                Self(NEXT.fetch_add(1, Ordering::Relaxed))
            }
        }
    };
}

counter_id!(
    /// The conversation driver, or a subagent it spawned.
    AgentId
);
counter_id!(
    /// One provider round trip and the tools it asked for.
    TurnId
);
counter_id!(
    /// One tool call.
    ToolCallId
);
counter_id!(
    /// One question a running turn is waiting on.
    UserRequestId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_ordered() {
        let first = AgentId::next();
        let second = AgentId::next();
        assert_ne!(first, second);
        assert!(first < second);
    }

    #[test]
    fn ids_survive_the_wire() {
        let id = ToolCallId(42);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "42");
        assert_eq!(serde_json::from_str::<ToolCallId>(&json).unwrap(), id);
    }
}

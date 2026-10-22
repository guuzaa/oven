//! What a frontend asks the app to do.
//!
//! One enum covers every request a frontend can make, so a transport carries
//! a single message type in each direction and the set of things a frontend
//! may ask never grows past what the app already answers.

use serde::{Deserialize, Serialize};

use crate::{AgentId, AgentMode, Input, TurnId, UserRequestId, UserResponse};

/// A request to do something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Command {
    /// Submit text. The app classifies it — a chat, a `!` shell line or a
    /// slash command — and reports what it took it for.
    Submit { text: String },
    /// Park a chat typed while a turn is running, so the next tool-result
    /// upload can carry it. Refused when the driver is idle.
    Steer { text: String },
    /// Drop the last turn from the conversation.
    Rewind,
    /// Stop the running turn.
    Cancel { turn_id: TurnId },
    /// Abort a compaction in progress.
    CancelCompaction,
    /// Switch the mode the next turn runs in.
    SetMode { mode: AgentMode },
    /// Answer the request a running turn is waiting on.
    Respond {
        request_id: UserRequestId,
        response: UserResponse,
    },
    /// Stop one subagent.
    StopSubagent { id: AgentId },
    /// Stop every subagent.
    StopSubagents,
}

/// What a command did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum CommandResult {
    /// The submission was classified; a frontend draws it the way the runtime
    /// will treat it.
    Submitted(Input),
    /// The command was applied; there is nothing to draw.
    Applied,
    /// The command did not apply: the driver was idle, or the request it
    /// answered was already closed.
    Refused,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_survives_the_wire() {
        let command = Command::Respond {
            request_id: UserRequestId(4),
            response: UserResponse::Approval(crate::ApprovalDecision::Approved),
        };
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"respond","request_id":4,"response":{"kind":"approval","value":"approved"}}"#
        );
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), command);
    }

    #[test]
    fn a_result_survives_the_wire() {
        let result = CommandResult::Submitted(Input::Chat("hi".into()));
        let json = serde_json::to_string(&result).unwrap();
        assert_eq!(
            serde_json::from_str::<CommandResult>(&json).unwrap(),
            result
        );
    }
}

use oven_agent::{AgentMode, TurnId};

/// Commands sent from a frontend to the runtime task.
///
/// `Prompt` and `Control` are kept structurally distinct so callers never
/// have to guess (via string-sniffing) whether a piece of text is a chat
/// message or a control instruction: that classification happens once,
/// inside the runtime, where the slash-command registry lives.
#[derive(Debug, Clone)]
pub enum AppCommand {
    Prompt(String),
    Control(ControlCommand),
    Shutdown,
}

/// Control-plane instructions that don't require exclusive access to the
/// agent while a turn is streaming. Anything that does (switching models,
/// clearing history, ...) is instead expressed as slash-command text routed
/// through `AppCommand::Prompt` and resolved by the runtime once the agent
/// is free.
#[derive(Debug, Clone)]
pub enum ControlCommand {
    Cancel { turn_id: TurnId },
    SetMode { mode: AgentMode },
    Rewind,
}

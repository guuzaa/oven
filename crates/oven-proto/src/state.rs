//! What the system is now.
//!
//! The state a frontend reads is split in two. [`AppLevels`] is small and
//! changes constantly — the phase, the mode, the model, the checklist — so a
//! frontend keeps it current from a stream of updates. [`AppState`] is the
//! whole picture, the conversation included, and is what a frontend reads
//! once when it connects.

use std::sync::Arc;

use oven_llm::{EffortSupport, Message, ProviderKind, ReasoningEffort, Usage};
use serde::{Deserialize, Serialize};

use crate::{AgentId, AgentMode, NodeInfo, TodoList};

/// The whole published state: the levels, plus the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    pub levels: AppLevels,
    /// Shared handles to the agent's messages: publishing state (and every
    /// `watch` clone it triggers) then costs refcounts, not the conversation.
    pub history: Vec<Arc<Message>>,
    /// Unix-ms timestamps parallel to `history`, taken from `Record`.
    pub history_timestamps: Vec<u64>,
    /// Thinking duration in ms parallel to `history`; `None` if that message
    /// had no timed thinking.
    pub history_thinking_ms: Vec<Option<u64>>,
}

/// Everything about the system that is not the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppLevels {
    pub phase: AppPhase,
    /// The conversation driver. Every other agent a view sees is a subagent,
    /// which is how an event is routed to the transcript it belongs to.
    pub agent_id: AgentId,
    /// Subagents in spawn order, mirrored from the registry. Shared, so
    /// publishing state and telling the frontend it moved cost a refcount
    /// rather than a copy of every subagent.
    pub subagents: Arc<Vec<NodeInfo>>,
    pub mode: AgentMode,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// The active provider, without its credentials.
    pub provider: ProviderView,
    pub configured_providers: Vec<String>,
    pub todos: TodoList,
    pub last_turn_usage: Usage,
    /// Tokens of the last response in the current turn that the context holds:
    /// its input plus the output that joins the history. Zero until a turn
    /// completes.
    pub context_tokens: u32,
    /// Context window of the active model, when known.
    pub context_window: Option<u32>,
    pub session: SessionState,
    pub models: Vec<ModelChoice>,
}

/// One model the app can run, as a frontend sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    /// The model slug, e.g. `deepseek/deepseek-v4-flash`.
    pub id: String,
    /// The vendor that serves it.
    pub provider: String,
    /// What the model can express: the levels it takes, or that it only has
    /// a switch. A frontend picks its reasoning effort from this, so the
    /// choice can never name a level the model does not have.
    pub effort_support: EffortSupport,
}

/// The active provider as a frontend may see it.
///
/// The API key never crosses the protocol boundary: a frontend is told
/// whether one is configured, not what it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderView {
    pub name: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub protocol: Option<ProviderKind>,
    /// Whether a usable API key is configured.
    pub has_api_key: bool,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl ProviderView {
    /// True when no API key is configured, so a session should open `/setup`
    /// instead of failing at startup.
    pub fn needs_setup(&self) -> bool {
        !self.has_api_key
    }
}

impl AppState {
    /// The conversation with its record timestamps and thinking durations,
    /// sharing the agent's messages so a transcript re-seed does not copy
    /// every message.
    pub fn history_timed_shared(&self) -> Vec<(Arc<Message>, u64, Option<u64>)> {
        self.history
            .iter()
            .cloned()
            .zip(self.history_timestamps.iter().copied())
            .zip(self.history_thinking_ms.iter().copied())
            .map(|((message, timestamp), thinking_ms)| (message, timestamp, thinking_ms))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppPhase {
    Idle,
    Running {
        turn_id: crate::TurnId,
    },
    /// The turn is blocked on the user: a tool approval, the loop limit or a
    /// question. What it is waiting for reaches a frontend as the agent event
    /// that announced it.
    Awaiting {
        turn_id: crate::TurnId,
    },
    Cancelling {
        turn_id: crate::TurnId,
    },
    /// The history is being summarized. Cancel aborts the summary request
    /// and leaves the session as it was.
    Compacting,
    ShuttingDown,
}

impl AppPhase {
    pub fn turn_id(&self) -> Option<crate::TurnId> {
        match self {
            Self::Running { turn_id }
            | Self::Awaiting { turn_id }
            | Self::Cancelling { turn_id } => Some(*turn_id),
            Self::Idle | Self::Compacting | Self::ShuttingDown => None,
        }
    }

    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    /// Whether the driver is occupied, so a frontend shows itself busy.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Running { .. }
                | Self::Awaiting { .. }
                | Self::Cancelling { .. }
                | Self::Compacting
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    pub id: Option<String>,
}

/// Why the conversation history was replaced, so a view can rebuild itself
/// and tell a rewind from a `/clear`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryChangeReason {
    /// Esc rewind truncated the last turn.
    Rewound,
    /// `/clear` dropped the conversation.
    Cleared,
    /// `/compact` or auto-compaction replaced it with a summary.
    Compacted,
    /// History was replaced by something outside a known command.
    External,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phase_survives_the_wire() {
        let phase = AppPhase::Running {
            turn_id: crate::TurnId(3),
        };
        let json = serde_json::to_string(&phase).unwrap();
        assert_eq!(json, r#"{"kind":"running","turn_id":3}"#);
        assert_eq!(serde_json::from_str::<AppPhase>(&json).unwrap(), phase);
    }

    #[test]
    fn a_provider_view_holds_no_credential() {
        let view = ProviderView {
            name: Some("deepseek".into()),
            has_api_key: true,
            ..ProviderView::default()
        };
        let json = serde_json::to_string(&view).unwrap();
        let fields: serde_json::Value = serde_json::from_str(&json).unwrap();
        let fields = fields.as_object().unwrap();
        assert!(
            !fields.contains_key("api_key"),
            "the credential must never reach the wire: {json}"
        );
        assert_eq!(
            fields.get("has_api_key"),
            Some(&serde_json::Value::Bool(true))
        );
        assert!(!view.needs_setup());
    }

    #[test]
    fn the_state_survives_the_wire() {
        let state = AppState {
            levels: AppLevels {
                phase: AppPhase::Idle,
                agent_id: AgentId(1),
                subagents: Arc::new(Vec::new()),
                mode: AgentMode::Agent,
                model: "deepseek-chat".into(),
                reasoning_effort: None,
                provider: ProviderView::default(),
                configured_providers: vec!["deepseek".into()],
                todos: TodoList::default(),
                last_turn_usage: Usage::default(),
                context_tokens: 0,
                context_window: None,
                session: SessionState::default(),
                models: vec![ModelChoice {
                    id: "deepseek-chat".into(),
                    provider: "DeepSeek".into(),
                    effort_support: EffortSupport::Unknown,
                }],
            },
            history: vec![Arc::new(Message::user_text("hi"))],
            history_timestamps: vec![1],
            history_thinking_ms: vec![None],
        };
        let json = serde_json::to_string(&state).unwrap();
        let decoded: AppState = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.levels.model, "deepseek-chat");
        assert_eq!(decoded.levels.phase, AppPhase::Idle);
        assert_eq!(decoded.levels.configured_providers, vec!["deepseek"]);
        assert_eq!(decoded.history.len(), 1);
        assert_eq!(decoded.history_timestamps, vec![1]);
    }
}

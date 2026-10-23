//! How the app publishes its state.
//!
//! The state shape belongs to `oven-proto`; what stays here is the rule that
//! reads it off the agent and the router.

use std::sync::Arc;

use oven_agent::Agent;
use oven_llm::{ModelId, Router};
use oven_proto::{AppLevels, AppPhase, AppState, ProviderView, SessionState};

/// The state an app starts with: the levels the agent and the provider
/// report, plus the conversation it already holds.
pub(crate) fn app_state(
    agent: &Agent,
    provider: ProviderView,
    configured_providers: Vec<String>,
    session: SessionState,
) -> AppState {
    let (history_timestamps, history_thinking_ms): (Vec<u64>, Vec<Option<u64>>) = agent
        .history_timed()
        .map(|(_, timestamp, thinking_ms)| (timestamp, thinking_ms))
        .unzip();
    AppState {
        levels: AppLevels {
            phase: AppPhase::Idle,
            agent_id: agent.id(),
            subagents: Arc::new(Vec::new()),
            mode: agent.mode(),
            model: agent.model().to_string(),
            reasoning_effort: agent.reasoning_effort(),
            provider,
            configured_providers,
            todos: agent.todos().clone(),
            last_turn_usage: agent.last_turn_usage(),
            context_tokens: context_tokens(agent),
            context_window: context_window(agent),
            session,
            models: Vec::new(),
        },
        history: agent.shared_history(),
        history_timestamps,
        history_thinking_ms,
    }
}

/// Tokens the last response in the current turn leaves in the context: the
/// input it sent (cache reads included) plus the output it added to the
/// history.
pub(crate) fn context_tokens(agent: &Agent) -> u32 {
    let usage = agent.last_turn_usage();
    usage.input_tokens.saturating_add(usage.output_tokens)
}

/// Context window of `model`, when the router knows it.
pub(crate) fn context_window_of(router: &Router, model: &str) -> Option<u32> {
    router
        .resolve_model(&ModelId::from(model))
        .map(|info| info.context_window)
        .filter(|window| *window > 0)
}

/// Context window of the agent's active model, when the router knows it.
pub(crate) fn context_window(agent: &Agent) -> Option<u32> {
    context_window_of(&agent.router(), agent.model().as_str())
}

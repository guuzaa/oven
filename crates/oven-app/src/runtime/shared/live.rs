//! What the user can do while a turn holds the conversation driver: switch
//! the model, look at the subagents, leave. Everything else waits its turn.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, MutexGuard, PoisonError};

use oven_llm::{ModelId, ReasoningEffort, Router};
use tokio::sync::watch;

use crate::command::Input;
use crate::commands::{CommandOutcome, Model, ModelDirective, SlashRegistry};
use crate::core::config::{AppConfig, ProviderConfig};
use crate::core::event::{AppEventKind, EventBus, SubagentEvent};
use crate::core::state::{AppState, context_window_of};

use super::Shared;

pub(crate) const GOODBYE: &str = "goodbye";
const QUEUED_NOTICE_SUFFIX: &str = "queued: will apply once the current reply finishes";
const REWIND_QUEUED_NOTICE: &str = "rewind queued: will apply once the current reply finishes";

impl Shared {
    pub(crate) fn config(&self) -> MutexGuard<'_, AppConfig> {
        self.config.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.state.borrow().phase.is_active()
    }

    fn router_snapshot(&self) -> Arc<Router> {
        self.router
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Applies `input` on the spot when it never asks for the driver,
    /// returning whether it did. Anything the input wants the driver for —
    /// `/clear`, `/setup`, `/compact` — reports itself busy and is left for
    /// the driver, so a turn in flight keeps the conversation to itself
    /// while a view command like `/agents` still works when the user needs
    /// it most.
    pub(crate) fn apply_now(&self, input: &Input, slash: &SlashRegistry) -> bool {
        let Input::Slash { name, args } = input else {
            return false;
        };
        if name == Model::NAME {
            self.model_command(args);
            return true;
        }
        match slash.run_shared(&self.subagents, name, args) {
            Ok(Some(outcome)) => {
                self.apply_shared_outcome(name, outcome);
                true
            }
            Ok(None) => false,
            Err(error) => {
                self.events.emit_error(error.to_string());
                true
            }
        }
    }

    /// Only outcomes a command can reach without the driver arrive here; the
    /// fallback keeps a command that forgot its `cx.agent()` guard from
    /// doing more than waiting.
    fn apply_shared_outcome(&self, name: &str, outcome: CommandOutcome) {
        match outcome {
            CommandOutcome::Reply(text) => self.notify(text),
            CommandOutcome::FocusSubagent { id } => {
                self.events
                    .emit(AppEventKind::Subagent(SubagentEvent::Focus { id }));
            }
            CommandOutcome::Exit => {
                self.subagents.shutdown();
                self.notify(GOODBYE.into());
                self.events.emit(AppEventKind::Exited);
            }
            _ => self
                .events
                .emit_error(format!("/{name} has to wait for the current reply")),
        }
    }

    pub(crate) fn notify(&self, text: String) {
        self.events.emit(AppEventKind::Notification { text });
    }

    pub(crate) fn model_command(&self, args: &str) {
        let (model, current_effort) = self.selection.model();
        match Model::resolve(&self.router_snapshot(), current_effort, args) {
            Ok(ModelDirective::Query) => {
                self.notify(Model::describe(model.as_str(), current_effort));
            }
            Ok(ModelDirective::Switch {
                model,
                reasoning_effort,
            }) => self.switch_model(model, reasoning_effort),
            Err(error) => self.events.emit_error(error.to_string()),
        }
    }

    pub(crate) fn switch_model(&self, model: String, reasoning_effort: Option<ReasoningEffort>) {
        let router = self.router_snapshot();
        let outcome = resolve_model_switch(&router, &mut self.config(), model, reasoning_effort);
        self.selection.switch_model(
            ModelId::from(outcome.model.as_str()),
            outcome.reasoning_effort,
        );
        self.state.send_modify(|state| {
            state.model.clone_from(&outcome.model);
            state.reasoning_effort = outcome.reasoning_effort;
        });
        set_context_window(&self.state, context_window_of(&router, &outcome.model));
        let saved = save_provider_overlay(
            self.user_config_path.as_deref(),
            &outcome.overlay,
            &self.events,
        );
        let mut text = format_model_switched(&outcome.model, outcome.reasoning_effort);
        if let Some(path) = saved {
            let _ = write!(text, "\nsaved to {}", path.display());
        }
        self.notify(text);
    }
}

/// The acknowledgement for an input that has to wait for the running turn,
/// so the UI never looks stuck while it sits in the queue.
pub(crate) fn queued_notice(input: &Input) -> Option<String> {
    match input {
        Input::Rewind => Some(REWIND_QUEUED_NOTICE.to_string()),
        Input::Slash { name, .. } => Some(format!("/{name} {QUEUED_NOTICE_SUFFIX}")),
        Input::Chat(_) | Input::Shell(_) => None,
    }
}

struct ModelSwitchOutcome {
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
    overlay: ProviderConfig,
}

/// Publishes the active model's context window, but only when it moved: a
/// frontend already holding the window learns nothing from a repeat.
pub(crate) fn set_context_window(state_tx: &watch::Sender<AppState>, window: Option<u32>) {
    state_tx.send_if_modified(|state| {
        let moved = state.context_window != window;
        state.context_window = window;
        moved
    });
}

/// Resolves a `/model` switch against `router`/`config` without touching
/// the agent, so it runs whether or not a turn holds it.
fn resolve_model_switch(
    router: &Router,
    config: &mut AppConfig,
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
) -> ModelSwitchOutcome {
    let id = ModelId::from(model.as_str());
    let name = id
        .vendor()
        .map(oven_llm::canonical_vendor)
        .or_else(|| {
            router
                .provider(&id)
                .ok()
                .map(|p| p.provider_name().slug().to_string())
        })
        .unwrap_or_else(|| config.active_provider.name.clone());
    let provider = config.providers.entry(name.clone()).or_insert_with(|| {
        let mut provider = ProviderConfig {
            name: Some(name.clone()),
            ..Default::default()
        };
        provider.apply_name_presets();
        provider
    });
    provider.model = Some(model.clone());
    if reasoning_effort.is_some() {
        provider.reasoning_effort = reasoning_effort;
    }
    provider.normalize();
    let reasoning_effort = provider.reasoning_effort;
    let overlay = provider.clone();
    config.active_provider.name = name;
    ModelSwitchOutcome {
        model,
        reasoning_effort,
        overlay,
    }
}

fn format_model_switched(model: &str, reasoning_effort: Option<ReasoningEffort>) -> String {
    match reasoning_effort {
        Some(e) => format!("model switched to {model} (effort: {e})"),
        None => format!("model switched to {model}"),
    }
}

/// Persists `overlay` to the user config file, if one is configured.
pub(crate) fn save_provider_overlay(
    user_config_path: Option<&Path>,
    overlay: &ProviderConfig,
    events: &EventBus,
) -> Option<PathBuf> {
    let path = user_config_path?;
    match AppConfig::save_provider_at(path, overlay) {
        Ok(()) => Some(path.to_path_buf()),
        Err(e) => {
            events.emit_error(e.to_string());
            None
        }
    }
}

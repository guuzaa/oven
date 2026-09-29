use std::collections::VecDeque;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use std::collections::HashSet;
use std::time::Duration;

use oven_agent::{
    Agent, AgentId, AgentMode, Record, RouterHandle, RunPolicy, TodoList, UserResponse,
};
use oven_llm::{
    ModelId, ModelInfo, Provider, ProviderError, ProviderName, ReasoningEffort, Router,
};
use tokio::sync::{mpsc, watch};
use tracing::Instrument;

use crate::App;
use crate::command::{AppCommand, ControlCommand};
use crate::config::{AppConfig, ProviderConfig};
use crate::event::{AppEventKind, AppId, CompactionEvent, EventBus, SubagentEvent};
use crate::session::{
    Session, SessionError, SessionStore, current_or_session_span, record_recent,
    record_session_span,
};
use crate::slash::{CommandOutcome, SlashRegistry};
use crate::state::{
    AppPhase, AppState, HistoryChangeReason, SessionState, StateChange, context_tokens,
    context_window,
};
use crate::subagent::Subagents;

const NOTHING_TO_COMPACT_NOTICE: &str = "nothing to compact";

/// One app's agents and the bus they report on.
///
/// The driver and every subagent publish straight to `events`. `wake` is the
/// separate signal that the subagent registry changed — cheap to send, and
/// never carrying a payload the runtime could get stale.
pub(crate) struct AppAgents {
    pub(crate) main: Agent,
    pub(crate) subagents: Arc<Subagents>,
    pub(crate) events: EventBus,
    pub(crate) wake_rx: mpsc::UnboundedReceiver<()>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Control {
    Continue,
    Shutdown,
}

pub(crate) struct Runtime {
    pub(crate) agent: Agent,
    pub(crate) subagents: Arc<Subagents>,
    /// "The subagent registry changed"; the snapshot is read on demand.
    pub(crate) wake_rx: mpsc::UnboundedReceiver<()>,
    /// How many queued prompts went unsent when the app shut down, for the
    /// frontend that queued them to report once it has the terminal back.
    pub(crate) unsent: Arc<AtomicUsize>,
    /// Independent of `&mut agent`, so `/model` can be validated and
    /// applied while a turn holds the agent's exclusive borrow.
    pub(crate) router: RouterHandle,
    pub(crate) root: PathBuf,
    pub(crate) state: AppState,
    pub(crate) state_tx: watch::Sender<AppState>,
    pub(crate) session: Option<SessionStore>,
    pub(crate) config: AppConfig,
    pub(crate) user_config_path: Option<PathBuf>,
    pub(crate) events: EventBus,
    pub(crate) slash: SlashRegistry,
    /// What one user turn may spend. Derived from config at startup so the
    /// same budget reaches every run the runtime starts.
    pub(crate) policy: RunPolicy,
    /// The subagent registry's revision as last mirrored into state, so a
    /// signal that carried no change is dropped without copying the list.
    subagent_revision: u64,
    /// Messages already written to the current session file; everything past
    /// it is appended after each turn.
    pub(crate) persisted_messages: usize,
    pub(crate) persisted_rev: u64,
    pub(crate) pending: VecDeque<AppCommand>,
}

impl Runtime {
    #[allow(clippy::too_many_arguments)]
    fn new(
        agents: AppAgents,
        root: PathBuf,
        session: Option<SessionStore>,
        config: AppConfig,
        user_config_path: Option<PathBuf>,
        state: AppState,
        state_tx: watch::Sender<AppState>,
        unsent: Arc<AtomicUsize>,
    ) -> Self {
        let persisted_messages = match &session {
            Some(store) if store.current().path().exists() => agents.main.history().len(),
            _ => 0,
        };
        let persisted_rev = agents.main.history_revision();
        let router = agents.main.router_handle();
        let policy = RunPolicy::default().with_max_iters(config.max_iters);
        let AppAgents {
            main: agent,
            subagents,
            events,
            wake_rx,
        } = agents;
        Self {
            agent,
            subagents,
            events,
            wake_rx,
            unsent,
            router,
            root,
            state,
            state_tx,
            session,
            config,
            user_config_path,
            slash: SlashRegistry::with_builtin(),
            policy,
            subagent_revision: 0,
            persisted_messages,
            persisted_rev,
            pending: VecDeque::new(),
        }
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<AppCommand>) {
        self.bootstrap().await;
        loop {
            // Subagent events go straight to the bus. A wake still has to be
            // mirrored into state while no turn of ours is running.
            let cmd = match self.pending.pop_front() {
                Some(cmd) => cmd,
                None => tokio::select! {
                    cmd = rx.recv() => match cmd {
                        Some(cmd) => cmd,
                        None => break,
                    },
                    Some(()) = self.wake_rx.recv() => {
                        self.sync_subagents();
                        continue;
                    }
                },
            };
            if let AppCommand::Shutdown = cmd {
                tracing::debug!(kind = "shutdown", "runtime command");
                self.shutdown();
                break;
            }
            if self.handle(cmd, &mut rx).await == Control::Shutdown {
                break;
            }
        }
    }

    async fn handle(
        &mut self,
        cmd: AppCommand,
        rx: &mut mpsc::UnboundedReceiver<AppCommand>,
    ) -> Control {
        tracing::debug!(kind = command_kind(&cmd), "runtime command");
        match cmd {
            AppCommand::Shutdown => Control::Shutdown,
            AppCommand::Control(ControlCommand::SetMode { mode }) => {
                self.set_mode(mode);
                Control::Continue
            }
            AppCommand::Control(ControlCommand::StopSubagent { id }) => {
                self.stop_subagent(id);
                Control::Continue
            }
            AppCommand::Control(ControlCommand::StopSubagents) => {
                self.stop_subagents();
                Control::Continue
            }
            AppCommand::Control(ControlCommand::Cancel { .. } | ControlCommand::Respond { .. }) => {
                Control::Continue
            }
            AppCommand::Control(ControlCommand::Rewind) => {
                self.rewind();
                Control::Continue
            }
            AppCommand::Prompt(input) => self.start_turn(input, rx).await,
        }
    }

    pub(crate) fn persist_turn(&mut self) {
        let errors = match self.session.as_ref() {
            None => return,
            Some(store) => {
                let mut errors = Vec::new();
                let rev = self.agent.history_revision();
                if rev == self.persisted_rev {
                    let pending = self.agent.history_records_from(self.persisted_messages);
                    if !pending.is_empty() {
                        if let Err(error) = store.current().append_records(&pending) {
                            errors.push(error.to_string());
                        } else {
                            store.mark_content(true);
                            self.persisted_messages = self.agent.history().len();
                            if let Err(error) = record_recent_path(store) {
                                errors.push(error.to_string());
                            }
                        }
                    }
                } else {
                    self.persisted_messages = 0;
                    self.persisted_rev = rev;
                }
                if should_persist_todos(self.agent.todos(), self.agent.todo_written_this_turn())
                    && let Err(error) = persist_todo_snapshot(store, self.agent.todos())
                {
                    errors.push(error.to_string());
                }
                errors
            }
        };
        for error in errors {
            self.emit_error(error);
        }
        self.sync_state();
        self.publish();
    }

    async fn bootstrap(&mut self) {
        let model = self.agent.model().to_string();
        let router = self.agent.router();
        let (models, _) = refresh_model_choices(router.as_ref(), &model, &self.config).await;
        self.state.models.clone_from(&models);
        self.publish();
        self.emit_state(StateChange::ModelsChanged { models });
    }

    fn shutdown(&mut self) {
        self.state.phase = AppPhase::ShuttingDown;
        self.subagents.shutdown();
        report_unsent(&self.pending, &self.unsent);
        self.publish();
    }

    pub(crate) fn emit(&self, kind: AppEventKind) {
        self.events.emit(kind);
    }

    pub(crate) fn emit_state(&self, change: StateChange) {
        self.events.emit_state(change);
    }

    pub(crate) fn emit_error(&self, message: impl Into<String>) {
        self.events.emit_error(message);
    }

    /// Refresh the context fields from the agent. Only a window that moved is
    /// worth telling a frontend about: prompt-side tokens travel with the
    /// turn's own usage reports, and the model is the one thing the window
    /// follows.
    fn emit_context_changed(&mut self) {
        self.state.context_tokens = context_tokens(&self.agent);
        publish_context_window(
            &mut self.state,
            &self.state_tx,
            context_window(&self.agent),
            &self.events,
        );
    }

    fn should_auto_compact(&self) -> bool {
        let threshold = self.config.compact_threshold;
        if threshold <= 0.0 {
            return false;
        }
        let Some(window) = context_window(&self.agent) else {
            return false;
        };
        f64::from(context_tokens(&self.agent)) >= threshold * f64::from(window)
    }

    /// Compact the conversation into a summary and start a fresh session
    /// file holding only the summary. Shared by `/compact` and the
    /// auto-compaction trigger; failures leave the history untouched.
    async fn compact_history(&mut self) {
        if self.agent.history().len() == 0 {
            self.emit(AppEventKind::Notification {
                text: NOTHING_TO_COMPACT_NOTICE.into(),
            });
            return;
        }
        self.emit(AppEventKind::Compaction(CompactionEvent::Started));
        match self.agent.compact().await {
            Ok(stats) => {
                self.switch_session();
                if let Some(store) = &self.session {
                    self.agent.ensure_session_meta(store.root.clone());
                }
                self.persist_compacted();
                self.sync_state();
                self.publish();
                self.emit_state(StateChange::HistoryChanged {
                    revision: self.agent.history_revision(),
                    reason: HistoryChangeReason::Compacted,
                });
                self.emit_state(StateChange::UsageChanged {
                    usage: self.state.last_turn_usage,
                });
                self.emit_context_changed();
                self.emit_state(StateChange::SessionChanged {
                    session_id: self.state.session.id.clone(),
                });
                self.emit(AppEventKind::Compaction(CompactionEvent::Completed {
                    before_tokens: stats.before_tokens,
                    after_tokens: stats.after_tokens,
                }));
                self.emit(AppEventKind::Notification {
                    text: format_compacted(stats.before_tokens, stats.after_tokens),
                });
            }
            Err(e) => {
                self.emit(AppEventKind::Compaction(CompactionEvent::Failed {
                    error: e.message.clone(),
                }));
                self.emit_error(format!("compact failed: {}", e.message));
            }
        }
    }

    /// Write the compacted history (summary message) into the freshly
    /// switched session file.
    fn persist_compacted(&mut self) {
        self.persisted_rev = self.agent.history_revision();
        self.persisted_messages = 0;
        let mut errors = Vec::new();
        if let Some(store) = &self.session {
            let recs = self.agent.history_records();
            match store.current().overwrite(&recs) {
                Ok(()) => {
                    store.mark_content(true);
                    self.persisted_messages = self.agent.history().len();
                    if let Err(e) = record_recent_path(store) {
                        errors.push(e.to_string());
                    }
                }
                Err(e) => errors.push(e.to_string()),
            }
            if !self.agent.todos().is_empty()
                && let Err(e) = persist_todo_snapshot(store, self.agent.todos())
            {
                errors.push(e.to_string());
            }
        }
        for error in errors {
            self.emit_error(error);
        }
    }

    fn stop_subagent(&mut self, id: AgentId) {
        stop_subagent(&self.subagents, &self.events, id);
        self.sync_subagents();
    }

    pub(crate) fn stop_subagents(&mut self) {
        stop_subagents(&self.subagents, &self.events);
        self.sync_subagents();
    }

    pub(crate) fn sync_subagents(&mut self) {
        sync_subagents(
            &self.subagents,
            &mut self.subagent_revision,
            &mut self.state,
            &self.events,
            &self.state_tx,
        );
    }

    pub(crate) fn publish(&self) {
        let _ = self.state_tx.send(self.state.clone());
    }

    pub(crate) fn sync_state(&mut self) {
        self.state.mode = self.agent.mode();
        self.state.model = self.agent.model().to_string();
        self.state.reasoning_effort = self.agent.reasoning_effort();
        self.state.history = self.agent.shared_history();
        self.state.history_timestamps = self.agent.history_timed().map(|(_, ts, _)| ts).collect();
        self.state.history_thinking_ms = self.agent.history_timed().map(|(_, _, th)| th).collect();
        self.state.todos = self.agent.todos().clone();
        self.state.last_turn_usage = self.agent.last_turn_usage();
        self.state.context_tokens = context_tokens(&self.agent);
        self.state.context_window = context_window(&self.agent);
        self.state.session.id = self.session.as_ref().and_then(SessionStore::session_id);
    }

    fn set_mode(&mut self, mode: AgentMode) {
        self.agent.set_mode(mode);
        self.state.mode = mode;
        self.publish();
        self.emit_state(StateChange::ModeChanged { mode });
    }

    pub(crate) async fn apply_slash(&mut self, outcome: CommandOutcome) {
        match outcome {
            CommandOutcome::Passthrough => {}
            CommandOutcome::Reply(text) => {
                self.emit(AppEventKind::Notification { text });
            }
            CommandOutcome::Cleared => self.clear_session(),
            CommandOutcome::Compact => self.compact_history().await,
            CommandOutcome::Exit => {
                self.emit(AppEventKind::Notification {
                    text: "goodbye".into(),
                });
                self.emit(AppEventKind::Exited);
            }
            CommandOutcome::ModelChanged {
                model,
                reasoning_effort,
            } => self.set_model(model, reasoning_effort),
            CommandOutcome::ProviderChanged { provider } => self.set_provider(provider).await,
            CommandOutcome::ModeChanged { mode } => {
                self.set_mode(mode);
                self.emit(AppEventKind::Notification {
                    text: format!("mode switched to {}", mode.label()),
                });
            }
            CommandOutcome::FocusSubagent { id } => {
                self.emit(AppEventKind::Subagent(SubagentEvent::Focus { id }));
            }
        }
    }

    fn clear_session(&mut self) {
        self.agent.clear_history();
        self.agent.set_todos(TodoList::default());
        self.subagents.clear();
        self.sync_subagents();
        self.switch_session();
        if let Some(store) = &self.session {
            self.agent.ensure_session_meta(store.root.clone());
        }
        self.persisted_messages = 0;
        self.persisted_rev = self.agent.history_revision();
        self.sync_state();
        self.publish();
        self.emit_state(StateChange::HistoryChanged {
            revision: self.agent.history_revision(),
            reason: HistoryChangeReason::Cleared,
        });
        self.emit_state(StateChange::TodosChanged {
            todos: self.state.todos.clone(),
        });
        self.emit_state(StateChange::UsageChanged {
            usage: self.state.last_turn_usage,
        });
        self.emit_context_changed();
        self.emit_state(StateChange::SessionChanged {
            session_id: self.state.session.id.clone(),
        });
        self.emit(AppEventKind::Notification {
            text: "history cleared".into(),
        });
    }

    fn set_model(&mut self, model: String, reasoning_effort: Option<ReasoningEffort>) {
        let router = self.agent.router();
        let outcome = resolve_model_switch(&router, &mut self.config, model, reasoning_effort);
        self.agent.set_model(&*outcome.model);
        self.agent.set_reasoning_effort(outcome.reasoning_effort);
        self.state.model.clone_from(&outcome.model);
        self.state.reasoning_effort = outcome.reasoning_effort;
        self.publish();
        self.emit_state(StateChange::ModelChanged {
            model: outcome.model.clone(),
            reasoning_effort: outcome.reasoning_effort,
        });
        self.emit_context_changed();
        let saved = self.save_provider_overlay(&outcome.overlay);
        let mut text = format_model_switched(&outcome.model, outcome.reasoning_effort);
        if let Some(path) = saved {
            let _ = write!(text, "\nsaved to {}", path.display());
        }
        self.emit(AppEventKind::Notification { text });
    }

    async fn set_provider(&mut self, mut overlay: ProviderConfig) {
        overlay.normalize();
        if let Some(name) = overlay.name.clone() {
            if let Some(saved) = self.config.providers.get(&name).cloned() {
                overlay.fill_missing(&saved);
                let mut presets = ProviderConfig {
                    name: Some(name),
                    ..Default::default()
                };
                presets.apply_name_presets();
                overlay.fill_missing(&presets);
            } else {
                overlay.apply_name_presets();
            }
        } else if let Some(current) = self.config.active_provider_config().cloned() {
            overlay.fill_missing(&current);
            overlay.name = current.name;
        } else {
            self.emit_error("no active provider configured");
            return;
        }
        if overlay.reasoning_effort.is_none() {
            overlay.reasoning_effort = Some(ReasoningEffort::Medium);
        }
        if overlay.needs_setup() {
            self.emit_error("api_key required; run /setup name=<provider> api_key=<key>");
            return;
        }
        let mut next = self.config.clone();
        let name = overlay
            .name
            .clone()
            .expect("provider name assigned before update");
        next.providers
            .entry(name.clone())
            .or_default()
            .merge_fields(&overlay);
        next.active_provider.name = name;
        let model = next
            .active_provider_config()
            .expect("active provider inserted before build")
            .effective_model();
        let active = next
            .active_provider_config()
            .expect("active provider inserted before build");
        if let Err(e) = crate::provider::build_client(active) {
            self.emit_error(e.to_string());
            return;
        }
        // Rebuild the whole router rather than upserting one entry: the swap
        // is a single snapshot replacement, so a subagent holding the old
        // router finishes its request on it instead of racing the mutation.
        let router = match crate::provider::build_router(&next) {
            Ok(router) => router,
            Err(e) => {
                self.emit_error(e.to_string());
                return;
            }
        };
        self.config = next;
        self.agent.replace_router(router);
        self.agent.set_model(model.clone());
        self.agent.set_reasoning_effort(
            self.config
                .active_provider_config()
                .and_then(|provider| provider.reasoning_effort),
        );
        let saved = self.save_provider_overlay(&overlay);
        self.state.provider = public_provider(
            self.config
                .active_provider_config()
                .expect("active provider exists after update"),
        );
        self.state.configured_providers = self.config.configured_providers();
        self.state.model.clone_from(&model);
        self.state.reasoning_effort = self.agent.reasoning_effort();
        self.publish();
        self.emit_state(StateChange::ProviderChanged {
            provider: self.state.provider.clone(),
            configured_providers: self.state.configured_providers.clone(),
        });
        self.emit_state(StateChange::ModelChanged {
            model: model.clone(),
            reasoning_effort: self.agent.reasoning_effort(),
        });
        self.emit_context_changed();
        let router = self.agent.router();
        let (models, auth_error) =
            refresh_model_choices(router.as_ref(), &model, &self.config).await;
        self.state.models.clone_from(&models);
        self.publish();
        self.emit_state(StateChange::ModelsChanged { models });
        self.emit(AppEventKind::Notification {
            text: summarize_setup(&overlay, saved.as_deref()),
        });
        if let Some(body) = auth_error {
            self.emit_error(format!("API key rejected: {body}"));
        }
    }

    fn rewind(&mut self) {
        let _ = self.agent.rewind_last_turn();
        let found = TodoList::from_history(self.agent.history());
        let restored = found.clone().unwrap_or_default();
        self.agent.set_todos(restored.clone());
        let store_ok = match &self.session {
            Some(store) => {
                let mut recs = self.agent.history_records();
                if found.is_some() {
                    recs.push(Record::TodoList {
                        timestamp: oven_host::now_ms(),
                        items: restored.items.clone(),
                    });
                }
                match store.current().overwrite(&recs) {
                    Ok(()) => {
                        store.mark_content(self.agent.history().len() != 0);
                        true
                    }
                    Err(e) => {
                        self.emit_error(e.to_string());
                        false
                    }
                }
            }
            None => true,
        };
        if store_ok {
            self.persisted_messages = self.agent.history().len();
        }
        self.persisted_rev = self.agent.history_revision();
        self.sync_state();
        self.publish();
        self.emit_state(StateChange::TodosChanged { todos: restored });
        self.emit_state(StateChange::UsageChanged {
            usage: self.state.last_turn_usage,
        });
        self.emit_context_changed();
        self.emit_state(StateChange::HistoryChanged {
            revision: self.agent.history_revision(),
            reason: HistoryChangeReason::Rewound,
        });
    }

    fn switch_session(&mut self) {
        if let Some(store) = &self.session {
            let id = uuid::Uuid::now_v7().to_string();
            match Session::open(&store.dir, &id) {
                Ok(next) => {
                    record_session_span(next.id());
                    store.set_current(next);
                }
                Err(e) => self.emit_error(e.to_string()),
            }
        }
    }

    fn save_provider_overlay(&mut self, overlay: &ProviderConfig) -> Option<PathBuf> {
        save_provider_overlay(self.user_config_path.as_deref(), overlay, &self.events)
    }
}

struct ModelSwitchOutcome {
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
    overlay: ProviderConfig,
}

/// Publishes the active model's context window, but only when it moved: a
/// frontend already holding the window learns nothing from a repeat, and
/// prompt-side tokens travel with the turn's usage reports instead.
fn publish_context_window(
    state: &mut AppState,
    state_tx: &watch::Sender<AppState>,
    window: Option<u32>,
    events: &EventBus,
) {
    if window == state.context_window {
        return;
    }
    state.context_window = window;
    let _ = state_tx.send(state.clone());
    events.emit_state(StateChange::ContextWindowChanged { window });
}

/// Resolves a `/model` switch against `router`/`config` without touching
/// `Agent`, so the same logic serves both the idle path (`Runtime::set_model`)
/// and the mid-turn path (`apply_model_during_turn`).
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

fn command_kind(cmd: &AppCommand) -> &'static str {
    match cmd {
        AppCommand::Prompt(_) => "prompt",
        AppCommand::Control(ControlCommand::Cancel { .. }) => "cancel",
        AppCommand::Control(ControlCommand::SetMode { .. }) => "set_mode",
        AppCommand::Control(ControlCommand::Respond { response, .. }) => match response {
            UserResponse::Approval(_) => "tool_approval",
            UserResponse::LoopLimit(_) => "loop_limit",
            UserResponse::Answer(_) => "question",
        },
        AppCommand::Control(ControlCommand::Rewind) => "rewind",
        AppCommand::Control(ControlCommand::StopSubagent { .. }) => "stop_subagent",
        AppCommand::Control(ControlCommand::StopSubagents) => "stop_subagents",
        AppCommand::Shutdown => "shutdown",
    }
}

fn format_compacted(before_tokens: u32, after_tokens: u32) -> String {
    format!("context compacted: {before_tokens} → {after_tokens} tokens")
}

fn format_model_switched(model: &str, reasoning_effort: Option<ReasoningEffort>) -> String {
    match reasoning_effort {
        Some(e) => format!("model switched to {model} (effort: {e})"),
        None => format!("model switched to {model}"),
    }
}

/// Persists `overlay` to the user config file, if one is configured.
fn save_provider_overlay(
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

/// Prompts deferred while a turn ran never got their turn. A frontend queued
/// some of the user's messages itself; these are the ones it had already
/// handed over, and they are counted here so the two can be reported as one
/// number. Takes the fields it needs rather than `&self`, because a running
/// turn holds the agent.
pub(crate) fn report_unsent(pending: &VecDeque<AppCommand>, unsent: &AtomicUsize) {
    let count = pending
        .iter()
        .filter(|cmd| matches!(cmd, AppCommand::Prompt(_)))
        .count();
    if count > 0 {
        tracing::info!(unsent = count, "queued prompts dropped at shutdown");
    }
    unsent.store(count, Ordering::Relaxed);
}

fn stop_subagent(subagents: &Subagents, events: &EventBus, id: AgentId) {
    if !subagents.cancel(id) {
        events.emit_error(format!("no subagent {id:?} to stop"));
    }
}

fn stop_subagents(subagents: &Subagents, events: &EventBus) {
    let stopped = subagents.active();
    subagents.cancel_all();
    events.emit(AppEventKind::Notification {
        text: format!("cancelled {stopped} subagents"),
    });
}

/// Mirrors the subagent registry into published state. The registry is the
/// truth; `revision` is what it read last, so a signal the registry did not
/// change is dropped without copying the list — a subagent reports on every
/// tool call it starts.
fn sync_subagents(
    subagents: &Subagents,
    revision: &mut u64,
    state: &mut AppState,
    events: &EventBus,
    state_tx: &watch::Sender<AppState>,
) {
    let current = subagents.revision();
    if current == *revision {
        return;
    }
    *revision = current;
    let snapshot = Arc::new(subagents.snapshot());
    state.subagents.clone_from(&snapshot);
    let _ = state_tx.send(state.clone());
    events.emit_state(StateChange::SubagentsChanged {
        subagents: snapshot,
    });
}

pub(crate) fn hydrate_session(agent: &mut Agent, prior: &[Record]) {
    agent.set_todos(TodoList::restore(prior, agent.history()));
}

pub(crate) fn spawn_runtime(
    app_id: AppId,
    agents: AppAgents,
    session: Option<Session>,
    root: PathBuf,
    config: AppConfig,
    user_config_path: Option<PathBuf>,
) -> App {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let subscribers = agents.events.subscribers();
    let slash_commands = SlashRegistry::with_builtin().commands();
    let provider = config
        .active_provider_config()
        .map(public_provider)
        .unwrap_or_default();
    let configured_providers = config.configured_providers();
    let span = current_or_session_span(session.as_ref().map(Session::id));
    let (session_store, session_state) = match session {
        Some(s) => {
            let has_content = agents.main.history().len() != 0;
            let id = has_content.then(|| s.id().to_string());
            (
                Some(SessionStore::new(s, &root, has_content)),
                SessionState { id },
            )
        }
        None => (None, SessionState { id: None }),
    };
    let state = AppState::from_agent(&agents.main, provider, configured_providers, session_state);
    let (state_tx, state_rx) = watch::channel(state.clone());
    let unsent = Arc::new(AtomicUsize::new(0));
    let runtime = Runtime::new(
        agents,
        root.clone(),
        session_store,
        config,
        user_config_path,
        state,
        state_tx,
        Arc::clone(&unsent),
    );
    let join = tokio::spawn(runtime.run(cmd_rx).instrument(span));
    App::new(
        app_id,
        cmd_tx,
        subscribers,
        join,
        slash_commands,
        root,
        state_rx,
        unsent,
    )
}

pub(crate) fn persist_todo_snapshot(
    store: &SessionStore,
    todos: &TodoList,
) -> Result<(), SessionError> {
    store.current().append_records(&[Record::TodoList {
        timestamp: oven_host::now_ms(),
        items: todos.items.clone(),
    }])
}

pub(crate) fn should_persist_todos(todos: &TodoList, written_this_turn: bool) -> bool {
    written_this_turn || !todos.is_empty()
}

pub(crate) fn record_recent_path(store: &SessionStore) -> Result<(), SessionError> {
    record_recent(&store.dir, Path::new(&store.root), store.current().id())
}

fn public_provider(provider: &ProviderConfig) -> ProviderConfig {
    ProviderConfig {
        api_key: None,
        ..provider.clone()
    }
}

fn summarize_setup(overlay: &ProviderConfig, saved: Option<&Path>) -> String {
    let mut parts = Vec::new();
    if let Some(n) = &overlay.name {
        parts.push(format!("name={n}"));
    }
    if let Some(p) = overlay.protocol {
        parts.push(format!("protocol={p}"));
    }
    if let Some(m) = &overlay.model {
        parts.push(format!("model={m}"));
    }
    if let Some(u) = &overlay.base_url {
        parts.push(format!("base_url={u}"));
    }
    if overlay.api_key.is_some() {
        parts.push("api_key=(set)".into());
    }
    if let Some(e) = overlay.reasoning_effort {
        parts.push(format!("reasoning_effort={e}"));
    }
    let mut text = if parts.is_empty() {
        "provider unchanged".into()
    } else {
        format!("provider updated ({})", parts.join(" "))
    };
    if let Some(path) = saved {
        let _ = write!(text, "\nsaved to {}", path.display());
    }
    text
}

async fn refresh_model_choices(
    provider: &dyn Provider,
    current_model: &str,
    config: &AppConfig,
) -> (Vec<(String, String)>, Option<String>) {
    let timeout = config.request_timeout().min(Duration::from_secs(5));
    let known = provider.known_models();
    let (dynamic, auth_error) = match tokio::time::timeout(timeout, provider.list_models()).await {
        Ok(Ok(list)) => (list, None),
        Ok(Err(ProviderError::Auth(body))) => (Vec::new(), Some(body)),
        _ => (Vec::new(), None),
    };
    let current_provider = ModelId::from(current_model)
        .vendor()
        .map_or_else(|| provider.provider_name(), ProviderName::from);
    (
        merge_model_choices(known, dynamic, current_model, &current_provider),
        auth_error,
    )
}

fn merge_model_choices(
    known: Vec<ModelInfo>,
    dynamic: Vec<ModelInfo>,
    current_model: &str,
    current_provider: &ProviderName,
) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let choices = std::iter::once((
        slug_without_variant(current_model),
        current_provider.clone(),
    ))
    .chain(
        known
            .into_iter()
            .map(|m| (slug_without_variant(&m.id), m.provider)),
    )
    .chain(
        dynamic
            .into_iter()
            .map(|m| (slug_without_variant(&m.id), m.provider)),
    );
    for (id, provider) in choices {
        if !id.is_empty() && seen.insert(id.clone()) {
            out.push((id, provider.to_string()));
        }
    }
    out
}

fn slug_without_variant(raw: &str) -> String {
    let id = ModelId::from(raw);
    match id.vendor() {
        Some(vendor) => format!("{}/{}", oven_llm::canonical_vendor(vendor), id.wire_id()),
        None => id.wire_id().to_string(),
    }
}

mod turn;

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;

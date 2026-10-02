use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use std::collections::HashSet;
use std::time::Duration;

use oven_agent::{Agent, Record, RunPolicy, TodoList};
use oven_llm::{ModelId, ModelInfo, Provider, ProviderError, ProviderName, ReasoningEffort};
use oven_mem::MemoryStore;
use tokio::sync::{mpsc, watch};
use tracing::Instrument;

use crate::App;
use crate::capabilities::subagent::Subagents;
use crate::commands::{CommandOutcome, SlashRegistry};
use crate::core::config::{AppConfig, ProviderConfig};
use crate::core::event::{AppEventKind, AppId, CompactionEvent, EventBus, SubagentEvent};
use crate::core::input::Input;
use crate::core::session::{
    Session, SessionError, SessionStore, current_or_session_span, record_recent,
    record_session_span,
};
use crate::core::state::{
    AppPhase, AppState, HistoryChangeReason, SessionState, context_tokens, context_window,
};
use crate::runtime::inbox::InboxReceiver;
use crate::runtime::shared::{GOODBYE, Shared, save_provider_overlay};

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
    pub(crate) memory: Option<Arc<MemoryStore>>,
}

pub(crate) struct Runtime {
    pub(crate) agent: Agent,
    pub(crate) shared: Arc<Shared>,
    /// "The subagent registry changed"; the snapshot is read on demand.
    pub(crate) wake_rx: mpsc::UnboundedReceiver<()>,
    pub(crate) root: PathBuf,
    pub(crate) session: Option<SessionStore>,
    pub(crate) slash: Arc<SlashRegistry>,
    /// The store `AppBuilder` loaded, when memory is enabled.
    pub(crate) memory: Option<Arc<MemoryStore>>,
    /// What one user turn may spend. Derived from config at startup so the
    /// same budget reaches every run the runtime starts.
    pub(crate) policy: RunPolicy,
    /// Messages already written to the current session file; everything past
    /// it is appended after each turn.
    pub(crate) persisted_messages: usize,
    pub(crate) persisted_rev: u64,
}

impl Runtime {
    fn new(
        agents: AppAgents,
        root: PathBuf,
        session: Option<SessionStore>,
        shared: Arc<Shared>,
        slash: Arc<SlashRegistry>,
    ) -> Self {
        let persisted_messages = match &session {
            Some(store) if store.current().path().exists() => agents.main.history().len(),
            _ => 0,
        };
        let persisted_rev = agents.main.history_revision();
        let policy = RunPolicy::default().with_max_iters(shared.config().max_iters);
        let AppAgents {
            main: agent,
            wake_rx,
            memory,
            ..
        } = agents;
        Self {
            agent,
            shared,
            wake_rx,
            root,
            session,
            slash,
            memory,
            policy,
            persisted_messages,
            persisted_rev,
        }
    }

    async fn run(mut self, mut inbox: InboxReceiver) {
        self.bootstrap().await;
        loop {
            // Subagent events go straight to the bus. A wake still has to be
            // mirrored into state while no turn of ours is running.
            let input = tokio::select! {
                biased;
                () = self.shared.shutdown.cancelled() => break,
                input = inbox.recv() => match input {
                    Some(input) => input,
                    None => break,
                },
                Some(()) = self.wake_rx.recv() => {
                    self.shared.sync_subagents();
                    continue;
                }
            };
            self.handle(input).await;
        }
        self.shutdown(&inbox);
    }

    async fn handle(&mut self, input: Input) {
        tracing::debug!(kind = input.kind(), "runtime input");
        match input {
            Input::Rewind => self.rewind().await,
            Input::Shell(command) if command.is_empty() => self.reject_empty_shell(),
            Input::Shell(command) => self.run_shell(command).await,
            Input::Slash { name, args } => self.run_slash(&name, &args).await,
            Input::Chat(text) => self.start_turn(text).await,
        }
    }

    pub(crate) async fn persist_turn(&mut self) {
        let errors = match self.session.as_ref() {
            None => return,
            Some(store) => {
                let mut errors = Vec::new();
                let rev = self.agent.history_revision();
                if rev == self.persisted_rev {
                    let pending = self.agent.history_records_from(self.persisted_messages);
                    if !pending.is_empty() {
                        let session = store.current();
                        if let Err(error) = session.append_records(&pending).await {
                            errors.push(error.to_string());
                        } else {
                            store.mark_content(true);
                            self.persisted_messages = self.agent.history().len();
                            if let Err(error) = record_recent_path(store).await {
                                errors.push(error.to_string());
                            }
                        }
                    }
                } else {
                    self.persisted_messages = 0;
                    self.persisted_rev = rev;
                }
                if should_persist_todos(self.agent.todos(), self.agent.todo_written_this_turn())
                    && let Err(error) = persist_todo_snapshot(store, self.agent.todos()).await
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
    }

    async fn bootstrap(&mut self) {
        let model = self.agent.model().to_string();
        let router = self.agent.router();
        let config = self.shared.config().clone();
        let (models, _) = refresh_model_choices(router.as_ref(), &model, &config).await;
        self.shared.state.send_modify(|state| state.models = models);
    }

    fn shutdown(&self, inbox: &InboxReceiver) {
        self.shared.set_phase(AppPhase::ShuttingDown);
        self.shared.subagents.shutdown();
        let unsent = inbox.unsent();
        if unsent > 0 {
            tracing::info!(unsent, "queued prompts dropped at shutdown");
        }
    }

    pub(crate) fn emit(&self, kind: AppEventKind) {
        self.shared.events.emit(kind);
    }

    fn emit_history_changed(&self, reason: HistoryChangeReason) {
        self.emit(AppEventKind::HistoryChanged { reason });
    }

    pub(crate) fn emit_error(&self, message: impl Into<String>) {
        self.shared.events.emit_error(message);
    }

    fn should_auto_compact(&self) -> bool {
        let threshold = self.shared.config().compact_threshold;
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
        self.shared.set_phase(AppPhase::Compacting);
        self.emit(AppEventKind::Compaction(CompactionEvent::Started));
        match self.agent.compact().await {
            Ok(stats) => {
                self.switch_session().await;
                if let Some(store) = &self.session {
                    self.agent.ensure_session_meta(store.root.clone());
                }
                self.persist_compacted().await;
                self.sync_state();
                self.emit_history_changed(HistoryChangeReason::Compacted);
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
        self.shared.set_phase(AppPhase::Idle);
    }

    /// Write the compacted history (summary message) into the freshly
    /// switched session file.
    async fn persist_compacted(&mut self) {
        self.persisted_rev = self.agent.history_revision();
        self.persisted_messages = 0;
        let mut errors = Vec::new();
        if let Some(store) = &self.session {
            let recs = self.agent.history_records();
            let session = store.current();
            match session.overwrite(&recs).await {
                Ok(()) => {
                    store.mark_content(true);
                    self.persisted_messages = self.agent.history().len();
                    if let Err(e) = record_recent_path(store).await {
                        errors.push(e.to_string());
                    }
                }
                Err(e) => errors.push(e.to_string()),
            }
            if !self.agent.todos().is_empty()
                && let Err(e) = persist_todo_snapshot(store, self.agent.todos()).await
            {
                errors.push(e.to_string());
            }
        }
        for error in errors {
            self.emit_error(error);
        }
    }

    fn session_id(&self) -> Option<String> {
        self.session.as_ref().and_then(SessionStore::session_id)
    }

    pub(crate) fn sync_state(&self) {
        let agent = &self.agent;
        let session_id = self.session_id();
        self.shared.state.send_modify(|state| {
            state.mode = agent.mode();
            state.model = agent.model().to_string();
            state.reasoning_effort = agent.reasoning_effort();
            state.history = agent.shared_history();
            state.history_timestamps = agent.history_timed().map(|(_, ts, _)| ts).collect();
            state.history_thinking_ms = agent.history_timed().map(|(_, _, th)| th).collect();
            state.todos = agent.todos().clone();
            state.last_turn_usage = agent.last_turn_usage();
            state.context_tokens = context_tokens(agent);
            state.context_window = context_window(agent);
            state.session.id = session_id;
        });
    }

    pub(crate) async fn apply_slash(&mut self, outcome: CommandOutcome) {
        match outcome {
            CommandOutcome::Passthrough => {}
            CommandOutcome::Reply(text) => {
                self.emit(AppEventKind::Notification { text });
            }
            CommandOutcome::Cleared => self.clear_session().await,
            CommandOutcome::Compact => self.compact_history().await,
            CommandOutcome::Exit => {
                self.emit(AppEventKind::Notification {
                    text: GOODBYE.into(),
                });
                self.emit(AppEventKind::Exited);
            }
            CommandOutcome::ModelChanged {
                model,
                reasoning_effort,
            } => self.shared.switch_model(model, reasoning_effort),
            CommandOutcome::ProviderChanged { provider } => self.set_provider(provider).await,
            CommandOutcome::ModeChanged { mode } => {
                self.shared.set_mode(mode);
                self.emit(AppEventKind::Notification {
                    text: format!("mode switched to {}", mode.label()),
                });
            }
            CommandOutcome::FocusSubagent { id } => {
                self.emit(AppEventKind::Subagent(SubagentEvent::Focus { id }));
            }
            CommandOutcome::Memory(action) => {
                let text = match &self.memory {
                    Some(store) => crate::memory::apply(store, action).await,
                    None => crate::memory::MEMORY_DISABLED.to_owned(),
                };
                self.emit(AppEventKind::Notification { text });
            }
        }
    }

    async fn clear_session(&mut self) {
        self.agent.clear_history();
        self.agent.set_todos(TodoList::default());
        self.shared.subagents.clear();
        self.shared.sync_subagents();
        self.switch_session().await;
        if let Some(store) = &self.session {
            self.agent.ensure_session_meta(store.root.clone());
        }
        self.persisted_messages = 0;
        self.persisted_rev = self.agent.history_revision();
        self.sync_state();
        self.emit_history_changed(HistoryChangeReason::Cleared);
        self.emit(AppEventKind::Notification {
            text: "history cleared".into(),
        });
    }

    async fn set_provider(&mut self, mut overlay: ProviderConfig) {
        overlay.normalize();
        if let Some(name) = overlay.name.clone() {
            if let Some(saved) = self.shared.config().providers.get(&name).cloned() {
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
        } else if let Some(current) = self.shared.config().active_provider_config().cloned() {
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
        let mut next = self.shared.config().clone();
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
        if let Err(e) = crate::core::provider::build_client(active) {
            self.emit_error(e.to_string());
            return;
        }
        // Rebuild the whole router rather than upserting one entry: the swap
        // is a single snapshot replacement, so a subagent holding the old
        // router finishes its request on it instead of racing the mutation.
        let router = match crate::core::provider::build_router(&next) {
            Ok(router) => router,
            Err(e) => {
                self.emit_error(e.to_string());
                return;
            }
        };
        *self.shared.config() = next.clone();
        self.agent.replace_router(router);
        self.agent.set_model(model.clone());
        self.agent.set_reasoning_effort(
            next.active_provider_config()
                .and_then(|provider| provider.reasoning_effort),
        );
        let saved = self.save_provider_overlay(&overlay);
        let provider = public_provider(
            next.active_provider_config()
                .expect("active provider exists after update"),
        );
        let configured_providers = next.configured_providers();
        self.shared.state.send_modify(|state| {
            state.provider = provider;
            state.configured_providers = configured_providers;
        });
        self.sync_state();
        let router = self.agent.router();
        let (models, auth_error) = refresh_model_choices(router.as_ref(), &model, &next).await;
        self.shared.state.send_modify(|state| state.models = models);
        self.emit(AppEventKind::Notification {
            text: summarize_setup(&overlay, saved.as_deref()),
        });
        if let Some(body) = auth_error {
            self.emit_error(format!("API key rejected: {body}"));
        }
    }

    async fn rewind(&mut self) {
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
                let session = store.current();
                match session.overwrite(&recs).await {
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
        self.emit_history_changed(HistoryChangeReason::Rewound);
    }

    async fn switch_session(&mut self) {
        if let Some(store) = &self.session {
            let id = uuid::Uuid::now_v7().to_string();
            match Session::open(&store.dir, &id).await {
                Ok(next) => {
                    record_session_span(next.id());
                    store.set_current(next);
                }
                Err(e) => self.emit_error(e.to_string()),
            }
        }
    }

    fn save_provider_overlay(&self, overlay: &ProviderConfig) -> Option<PathBuf> {
        save_provider_overlay(self.shared.user_config_path(), overlay, &self.shared.events)
    }
}

fn format_compacted(before_tokens: u32, after_tokens: u32) -> String {
    format!("context compacted: {before_tokens} → {after_tokens} tokens")
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
    let (inbox_tx, inbox_rx) = inbox::channel();
    let subscribers = agents.events.subscribers();
    let slash = Arc::new(SlashRegistry::with_builtin());
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
    let (state_tx, _) = watch::channel(state);
    let shared = Arc::new(Shared::new(
        state_tx,
        agents.events.clone(),
        Arc::clone(&agents.subagents),
        &agents.main,
        config,
        user_config_path,
    ));
    let runtime = Runtime::new(
        agents,
        root.clone(),
        session_store,
        Arc::clone(&shared),
        Arc::clone(&slash),
    );
    let join = tokio::spawn(runtime.run(inbox_rx).instrument(span));
    App::new(app_id, inbox_tx, subscribers, join, slash, root, shared)
}

pub(crate) async fn persist_todo_snapshot(
    store: &SessionStore,
    todos: &TodoList,
) -> Result<(), SessionError> {
    let session = store.current();
    session
        .append_records(&[Record::TodoList {
            timestamp: oven_host::now_ms(),
            items: todos.items.clone(),
        }])
        .await
}

pub(crate) fn should_persist_todos(todos: &TodoList, written_this_turn: bool) -> bool {
    written_this_turn || !todos.is_empty()
}

pub(crate) async fn record_recent_path(store: &SessionStore) -> Result<(), SessionError> {
    let id = store.current().id().to_string();
    record_recent(&store.dir, Path::new(&store.root), &id).await
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

pub(crate) mod inbox;
pub(crate) mod shared;
mod turn;

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;

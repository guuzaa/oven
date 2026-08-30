use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oven_agent::{Agent, AgentMode, Record, TodoList, restore_todos};
use oven_llm::{
    Message, ModelId, ModelInfo, Provider, ProviderError, ProviderName, ReasoningEffort,
};
use tokio::sync::{mpsc, watch};

use crate::App;
use crate::command::AppCommand;
use crate::config::{AppConfig, ProviderConfig};
use crate::event::{AppEvent, AppEventKind, AppId, Subscribers};
use crate::session::{Session, SessionError, SessionStore, record_recent};
use crate::slash::{CommandOutcome, SlashRegistry};
use crate::state::{AppPhase, AppState, SessionState, StateChange, StateEvent};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Control {
    Continue,
    Shutdown,
}

pub(crate) struct Runtime {
    pub(crate) agent: Agent,
    pub(crate) state: AppState,
    pub(crate) state_tx: watch::Sender<AppState>,
    pub(crate) session: Option<SessionStore>,
    pub(crate) config: AppConfig,
    pub(crate) user_config_path: Option<PathBuf>,
    pub(crate) subscribers: Subscribers,
    pub(crate) slash: SlashRegistry,
    pub(crate) seq: u64,
    pub(crate) state_rev: u64,
    pub(crate) persisted_prefix: usize,
    pub(crate) persisted_rev: u64,
    pub(crate) pending: VecDeque<AppCommand>,
}

impl Runtime {
    fn new(
        agent: Agent,
        session: Option<SessionStore>,
        config: AppConfig,
        user_config_path: Option<PathBuf>,
        subscribers: Subscribers,
        state: AppState,
        state_tx: watch::Sender<AppState>,
    ) -> Self {
        let persisted_prefix = match &session {
            Some(store) if store.current().path().exists() => agent.history_records().len(),
            _ => 0,
        };
        let persisted_rev = agent.history_revision();
        Self {
            agent,
            state,
            state_tx,
            session,
            config,
            user_config_path,
            subscribers,
            slash: SlashRegistry::with_builtin(),
            seq: 0,
            state_rev: 0,
            persisted_prefix,
            persisted_rev,
            pending: VecDeque::new(),
        }
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<AppCommand>) {
        self.bootstrap().await;
        loop {
            let cmd = match self.pending.pop_front() {
                Some(cmd) => cmd,
                None => match rx.recv().await {
                    Some(cmd) => cmd,
                    None => break,
                },
            };
            if let AppCommand::Shutdown = cmd {
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
        match cmd {
            AppCommand::Shutdown => Control::Shutdown,
            AppCommand::Cancel { .. } => Control::Continue,
            AppCommand::SetMode { mode } => {
                self.set_mode(mode);
                Control::Continue
            }
            AppCommand::Rewind => {
                self.rewind();
                Control::Continue
            }
            AppCommand::ClearSession => {
                self.clear_session();
                Control::Continue
            }
            AppCommand::SetModel {
                model,
                reasoning_effort,
            } => {
                self.set_model(model, reasoning_effort);
                Control::Continue
            }
            AppCommand::SetProvider { provider } => {
                self.set_provider(provider).await;
                Control::Continue
            }
            AppCommand::StartTurn { input } => self.start_turn(input, rx).await,
        }
    }

    async fn bootstrap(&mut self) {
        let model = self.agent.model().to_string();
        let (models, _) = refresh_model_choices(self.agent.router(), &model, &self.config).await;
        self.state.models = models.clone();
        self.publish();
        self.emit_state(StateChange::ModelsChanged { models });
    }

    fn shutdown(&mut self) {
        self.state.phase = AppPhase::ShuttingDown;
        self.publish();
    }

    fn emit(&mut self, kind: AppEventKind) {
        emit(&mut self.seq, &self.subscribers, kind);
    }

    fn emit_state(&mut self, change: StateChange) {
        emit_state(
            &mut self.seq,
            &mut self.state_rev,
            &self.subscribers,
            change,
        );
    }

    pub(crate) fn emit_error(&mut self, message: impl Into<String>) {
        emit_error(&mut self.seq, &self.subscribers, message);
    }

    pub(crate) fn publish(&self) {
        publish(&self.state_tx, &self.state);
    }

    pub(crate) fn snapshot_agent(&mut self) {
        self.state.mode = self.agent.mode();
        self.state.model = self.agent.model().to_string();
        self.state.reasoning_effort = self.agent.reasoning_effort();
        self.state.history = self.agent.history().cloned().collect();
        self.state.todos = self.agent.todos().clone();
        self.state.last_turn_usage = self.agent.last_turn_usage();
    }

    pub(crate) fn sync_session_id(&mut self) {
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
        }
    }

    fn clear_session(&mut self) {
        self.agent.clear_history();
        self.agent.set_todos(TodoList::default());
        self.switch_session();
        if let Some(store) = &self.session {
            self.agent.ensure_session_meta(store.root.clone());
        }
        self.persisted_prefix = 0;
        self.persisted_rev = self.agent.history_revision();
        self.snapshot_agent();
        self.sync_session_id();
        self.publish();
        self.emit_state(StateChange::HistoryChanged {
            revision: self.agent.history_revision(),
        });
        self.emit_state(StateChange::TodosChanged {
            todos: self.state.todos.clone(),
        });
        self.emit_state(StateChange::UsageChanged {
            usage: self.state.last_turn_usage,
        });
        self.emit_state(StateChange::SessionChanged {
            session_id: self.state.session.id.clone(),
        });
        self.emit(AppEventKind::Notification {
            text: "history cleared".into(),
        });
    }

    fn set_model(&mut self, model: String, reasoning_effort: Option<ReasoningEffort>) {
        let id = ModelId::from(model.as_str());
        let name = id
            .vendor()
            .map(oven_llm::canonical_vendor)
            .or_else(|| {
                self.agent
                    .router()
                    .provider(&id)
                    .ok()
                    .map(|p| p.provider_name().slug().to_string())
            })
            .unwrap_or_else(|| self.config.active_provider.name.clone());
        let provider = self
            .config
            .providers
            .entry(name.clone())
            .or_insert_with(|| {
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
        let effective_reasoning_effort = provider.reasoning_effort;
        self.config.active_provider.name = name;
        self.agent.set_model(&*model);
        self.agent.set_reasoning_effort(effective_reasoning_effort);
        let overlay = provider.clone();
        self.state.model = model.clone();
        self.state.reasoning_effort = effective_reasoning_effort;
        self.publish();
        self.emit_state(StateChange::ModelChanged {
            model: model.clone(),
            reasoning_effort: effective_reasoning_effort,
        });
        let saved = self.save_provider_overlay(&overlay);
        let mut text = match effective_reasoning_effort {
            Some(e) => format!("model switched to {model} (effort: {e})"),
            None => format!("model switched to {model}"),
        };
        if let Some(path) = saved {
            text.push_str(&format!("\nsaved to {}", path.display()));
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
        match crate::provider::build_client(
            next.active_provider_config()
                .expect("active provider inserted before build"),
        ) {
            Ok(client) => {
                self.config = next;
                self.agent
                    .router_mut()
                    .upsert(crate::provider::retrying(&self.config, client));
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
                self.state.model = model.clone();
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
                let (models, auth_error) =
                    refresh_model_choices(self.agent.router(), &model, &self.config).await;
                self.state.models = models.clone();
                self.publish();
                self.emit_state(StateChange::ModelsChanged { models });
                self.emit(AppEventKind::Notification {
                    text: summarize_setup(&overlay, saved.as_deref()),
                });
                if let Some(body) = auth_error {
                    self.emit_error(format!("API key rejected: {body}"));
                }
            }
            Err(e) => self.emit_error(e.to_string()),
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
                        timestamp: now_ms(),
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
            self.persisted_prefix = self.agent.history_records().len();
        }
        self.persisted_rev = self.agent.history_revision();
        self.snapshot_agent();
        self.sync_session_id();
        self.publish();
        self.emit_state(StateChange::TodosChanged { todos: restored });
        self.emit_state(StateChange::UsageChanged {
            usage: self.state.last_turn_usage,
        });
        self.emit_state(StateChange::HistoryChanged {
            revision: self.agent.history_revision(),
        });
    }

    fn switch_session(&mut self) {
        if let Some(store) = &self.session {
            let id = uuid::Uuid::now_v7().to_string();
            match Session::open(&store.dir, &id) {
                Ok(next) => store.set_current(next),
                Err(e) => self.emit_error(e.to_string()),
            }
        }
    }

    fn save_provider_overlay(&mut self, overlay: &ProviderConfig) -> Option<PathBuf> {
        let path = self.user_config_path.as_deref()?;
        match AppConfig::save_provider_at(path, overlay) {
            Ok(()) => Some(path.to_path_buf()),
            Err(e) => {
                self.emit_error(e.to_string());
                None
            }
        }
    }
}

pub(crate) fn hydrate_session(agent: &mut Agent, prior: &[Record]) {
    agent.set_todos(restore_todos(prior, agent.history()));
}

pub(crate) fn spawn_runtime(
    app_id: AppId,
    agent: Agent,
    session: Option<Session>,
    root: PathBuf,
    config: AppConfig,
    user_config_path: Option<PathBuf>,
) -> App {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
    let subscribers_task = subscribers.clone();
    let slash_commands = SlashRegistry::with_builtin().commands();
    let history: Vec<Message> = agent.history().cloned().collect();
    let todos = agent.todos().clone();
    let last_turn_usage = agent.last_turn_usage();
    let provider = config
        .active_provider_config()
        .map(public_provider)
        .unwrap_or_default();
    let configured_providers = config.configured_providers();
    let mode = agent.mode();
    let model = agent.model().to_string();
    let reasoning_effort = agent.reasoning_effort();
    let (session_store, session_state) = match session {
        Some(s) => {
            let has_content = agent.history().len() != 0;
            let id = has_content.then(|| s.id().to_string());
            (
                Some(SessionStore::new(s, &root, has_content)),
                SessionState { id },
            )
        }
        None => (None, SessionState { id: None }),
    };
    let state = AppState {
        phase: AppPhase::Idle,
        mode,
        model,
        reasoning_effort,
        provider,
        configured_providers,
        history,
        todos,
        last_turn_usage,
        session: session_state,
        models: Vec::new(),
    };
    let (state_tx, state_rx) = watch::channel(state.clone());
    let runtime = Runtime::new(
        agent,
        session_store,
        config,
        user_config_path,
        subscribers_task,
        state,
        state_tx,
    );
    let join = tokio::spawn(async move {
        runtime.run(cmd_rx).await;
    });
    App::new(
        app_id,
        cmd_tx,
        subscribers,
        join,
        slash_commands,
        root,
        state_rx,
    )
}

pub(crate) fn emit(seq: &mut u64, subs: &Subscribers, kind: AppEventKind) {
    *seq += 1;
    let event = AppEvent { seq: *seq, kind };
    let mut subs = subs.lock().unwrap_or_else(|e| e.into_inner());
    subs.retain(|tx| tx.send(event.clone()).is_ok());
}

pub(crate) fn publish(state_tx: &watch::Sender<AppState>, state: &AppState) {
    let _ = state_tx.send(state.clone());
}

pub(crate) fn emit_state(seq: &mut u64, rev: &mut u64, subs: &Subscribers, change: StateChange) {
    *rev += 1;
    emit(
        seq,
        subs,
        AppEventKind::StateChanged(StateEvent {
            revision: *rev,
            change,
        }),
    );
}

pub(crate) fn emit_error(seq: &mut u64, subs: &Subscribers, message: impl Into<String>) {
    emit(
        seq,
        subs,
        AppEventKind::Error {
            message: message.into(),
        },
    );
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn persist_todo_snapshot(
    store: &SessionStore,
    todos: &TodoList,
) -> Result<(), SessionError> {
    store.current().append_records(&[Record::TodoList {
        timestamp: now_ms(),
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
        text.push_str(&format!("\nsaved to {}", path.display()));
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
        .map(ProviderName::from)
        .unwrap_or_else(|| provider.provider_name());
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
    use std::collections::HashSet;
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

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;

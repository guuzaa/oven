use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oven_agent::{
    Agent, AgentEvent, AgentMode, CancellationToken, LiveHandle, Record, TodoList, restore_todos,
};
use oven_llm::{
    ContentBlock, Message, ModelId, ModelInfo, Provider, ProviderError, ProviderName,
    ReasoningEffort, Usage,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::AppError;
use crate::config::{AppConfig, ProviderConfig};
use crate::event::{AppEvent, AppId, Subscribers};
use crate::session::{Session, canonical_root, record_recent};
use crate::slash::{CommandOutcome, SlashRegistry};

/// Commands sent from TUI / CLI into an app task.
#[derive(Debug, Clone)]
pub enum AppCmd {
    UserInput(String),
    Cancel,
    /// Drop the last user turn from the conversation (in-memory history and
    /// the persisted session file) so its message can be edited and resent.
    Rewind,
    Shutdown,
    SetMode(AgentMode),
}

/// Snapshot of the current persisted session, shared between the runtime
/// task and its `AppHandle`. The runtime task owns the source of truth and
/// updates this whenever the session switches or its content changes.
struct SharedSession {
    session: Session,
    has_content: bool,
}

/// Persisted session tracking for one runtime. `/clear` replaces the shared
/// current session with a fresh uuid v7 session so the old file is left
/// untouched.
struct SessionStore {
    dir: PathBuf,
    root: String,
    shared: Arc<Mutex<SharedSession>>,
}

impl SessionStore {
    /// The current session (id + path), cloned out of the shared snapshot.
    fn current(&self) -> Session {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .session
            .clone()
    }

    /// Replace the current session; a fresh session starts without content.
    fn set_current(&self, session: Session) {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.session = session;
        shared.has_content = false;
    }

    /// Update whether the current session holds conversation content.
    fn mark_content(&self, has_content: bool) {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_content = has_content;
    }
}

/// Handle to a running oven-app actor.
pub struct AppHandle {
    id: AppId,
    cmd_tx: mpsc::UnboundedSender<AppCmd>,
    subscribers: Subscribers,
    join: JoinHandle<()>,
    slash_commands: Vec<(String, String)>,
    model: String,
    provider: ProviderConfig,
    root: PathBuf,
    /// Cumulative token usage at spawn time: the restored usage of a resumed
    /// session, or zero for a fresh one. Startup snapshot, updated afterwards
    /// via `AgentEvent::Done` / `AppEvent::Rewound`.
    total_usage: Usage,
    /// Conversation history snapshot taken when the runtime was spawned.
    history: Vec<Message>,
    /// TODO list snapshot taken when the runtime was spawned.
    todos: TodoList,
    /// Current persisted session snapshot, `None` when this runtime has no
    /// session persistence. Updated live when `/clear` switches sessions.
    session: Option<Arc<Mutex<SharedSession>>>,
}

impl AppHandle {
    pub fn id(&self) -> AppId {
        self.id
    }

    pub fn send(&self, cmd: AppCmd) -> Result<(), AppError> {
        self.cmd_tx.send(cmd).map_err(|_| AppError::ChannelClosed)
    }

    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<AppEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    /// (name, description) pairs for the registered slash commands.
    pub fn slash_commands(&self) -> &[(String, String)] {
        &self.slash_commands
    }

    /// Model name in effect for this runtime.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Provider config snapshot taken at spawn time.
    pub fn provider_config(&self) -> &ProviderConfig {
        &self.provider
    }

    /// Workspace root the runtime was started with.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Messages loaded at spawn time (empty for a fresh session). This is a
    /// startup snapshot, not a live view of the in-flight conversation.
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// TODO list loaded at spawn time (empty for a fresh session). This is a
    /// startup snapshot; later changes arrive as `AgentEvent::TodoUpdated`.
    pub fn todos(&self) -> &TodoList {
        &self.todos
    }

    /// Token usage loaded at spawn time (zero for a fresh session). This is a
    /// startup snapshot; later changes arrive as events.
    pub fn total_usage(&self) -> Usage {
        self.total_usage
    }

    /// Current persisted session id for this runtime. Returns `None` when the
    /// runtime was spawned without session persistence, or when the current
    /// session has no conversation content yet (nothing was ever written to
    /// it). Tracks `/clear` switches.
    pub fn session_id(&self) -> Option<String> {
        let shared = self.session.as_ref()?;
        let shared = shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.has_content.then(|| shared.session.id().to_string())
    }

    /// Send one user turn and wait until the app returns to [`AppEvent::Idle`].
    /// Returns the final assistant text from [`AgentEvent::Done`] or a slash
    /// [`AppEvent::Reply`] when present.
    pub async fn prompt(&self, input: impl Into<String>) -> Result<String, AppError> {
        let mut rx = self.subscribe();
        self.send(AppCmd::UserInput(input.into()))?;

        let mut text = String::new();
        loop {
            match rx.recv().await {
                Some(AppEvent::Agent {
                    event: AgentEvent::Done { text: t, .. },
                    ..
                }) => text = t,
                Some(AppEvent::Notify { text: t, .. }) => text = t,
                Some(AppEvent::Idle { .. }) => return Ok(text),
                Some(AppEvent::Error { message, .. }) => {
                    return Err(AppError::Runtime(message));
                }
                Some(_) => {}
                None => return Err(AppError::ChannelClosed),
            }
        }
    }

    /// Request shutdown and wait for the app task to finish.
    pub async fn shutdown(self) {
        let _ = self.cmd_tx.send(AppCmd::Shutdown);
        let _ = self.join.await;
    }
}

pub(crate) fn hydrate_session(agent: &mut Agent, prior: &[Record]) {
    agent.set_todos(restore_todos(prior, agent.history()));
}

pub(crate) fn resolve_session(
    sessions_dir: &Path,
    session_id: Option<&str>,
) -> Result<Session, AppError> {
    let session = match session_id {
        Some(id) => {
            let candidate = Session::open(sessions_dir, id)?;
            if candidate.path().exists() {
                candidate
            } else {
                let uuid = uuid::Uuid::now_v7().to_string();
                Session::open(sessions_dir, &uuid)?
            }
        }
        None => {
            let uuid = uuid::Uuid::now_v7().to_string();
            Session::open(sessions_dir, &uuid)?
        }
    };
    Ok(session)
}

pub(crate) fn spawn_runtime(
    app_id: AppId,
    agent: Agent,
    session: Option<Session>,
    root: PathBuf,
    config: AppConfig,
    user_config_path: Option<PathBuf>,
) -> AppHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let subscribers: Subscribers = Arc::new(Mutex::new(Vec::new()));
    let subscribers_task = subscribers.clone();
    let slash_commands = SlashRegistry::with_builtin().commands();
    let history: Vec<Message> = agent.history().cloned().collect();
    let todos = agent.todos();
    let total_usage = *agent.total_usage();
    let provider = config.provider.clone();
    let (session_store, shared_session) = match session {
        Some(s) => {
            let shared = Arc::new(Mutex::new(SharedSession {
                session: s.clone(),
                has_content: agent.history().len() != 0,
            }));
            let dir = s.path().parent().map(Path::to_path_buf).unwrap_or_default();
            (
                Some(SessionStore {
                    dir,
                    root: canonical_root(&root),
                    shared: shared.clone(),
                }),
                Some(shared),
            )
        }
        None => (None, None),
    };
    let model = agent.model().to_string();
    let join = tokio::spawn(async move {
        runtime_loop(
            app_id,
            agent,
            session_store,
            cmd_rx,
            subscribers_task,
            config,
            user_config_path,
        )
        .await;
    });
    AppHandle {
        id: app_id,
        cmd_tx,
        subscribers,
        join,
        slash_commands,
        model,
        provider,
        root,
        total_usage,
        history,
        todos,
        session: shared_session,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn persist_todo_snapshot(
    store: &SessionStore,
    todos: &TodoList,
) -> Result<(), crate::session::SessionError> {
    store.current().append_records(&[Record::TodoList {
        timestamp: now_ms(),
        items: todos.items.clone(),
    }])
}

fn should_persist_todos(todos: &TodoList, written_this_turn: bool) -> bool {
    written_this_turn || !todos.is_empty()
}

fn apply_mode(live: &LiveHandle, mode: AgentMode) {
    live.lock().unwrap_or_else(|e| e.into_inner()).mode = mode;
}

fn emit(subs: &Subscribers, event: AppEvent) {
    let mut subs = subs.lock().unwrap_or_else(|e| e.into_inner());
    subs.retain(|tx| tx.send(event.clone()).is_ok());
}

fn emit_agent(subs: &Subscribers, app_id: AppId, event: AgentEvent) {
    emit(subs, AppEvent::Agent { app_id, event });
}

fn emit_done(subs: &Subscribers, app_id: AppId, agent: &Agent, text: String) {
    emit_agent(
        subs,
        app_id,
        AgentEvent::Done {
            agent_id: agent.id(),
            text,
            usage: *agent.total_usage(),
        },
    );
}

#[allow(clippy::too_many_arguments)]
async fn apply_slash(
    outcome: CommandOutcome,
    app_id: AppId,
    agent: &mut Agent,
    session_store: &mut Option<SessionStore>,
    persisted_prefix: &mut usize,
    persisted_rev: &mut u64,
    subs: &Subscribers,
    config: &mut AppConfig,
    user_config_path: Option<&Path>,
) {
    match outcome {
        CommandOutcome::Passthrough => {}
        CommandOutcome::Reply(text) => {
            emit(subs, AppEvent::Notify { app_id, text });
        }
        CommandOutcome::Cleared => {
            emit_done(subs, app_id, agent, "history cleared".to_string());
            emit_agent(
                subs,
                app_id,
                AgentEvent::HistoryCleared {
                    agent_id: agent.id(),
                },
            );
            emit_agent(
                subs,
                app_id,
                AgentEvent::TodoUpdated {
                    agent_id: agent.id(),
                    items: Vec::new(),
                },
            );
            switch_session(session_store, subs, app_id);
            if let Some(store) = session_store {
                agent.ensure_session_meta(store.root.clone());
            }
            *persisted_prefix = 0;
            *persisted_rev = agent.history_revision();
        }
        CommandOutcome::Exit => {
            emit_done(subs, app_id, agent, "goodbye".to_string());
            emit(subs, AppEvent::Exit { app_id });
        }
        CommandOutcome::ModelChanged {
            model,
            reasoning_effort,
        } => {
            agent.set_model(&*model);
            agent.set_reasoning_effort(reasoning_effort);
            config.provider.model = Some(model.clone());
            config.provider.reasoning_effort = reasoning_effort;
            emit_agent(
                subs,
                app_id,
                AgentEvent::ModelChanged {
                    agent_id: agent.id(),
                    model: model.clone(),
                    reasoning_effort,
                },
            );
            let overlay = ProviderConfig {
                model: Some(model.clone()),
                reasoning_effort,
                ..Default::default()
            };
            let saved = save_provider_overlay(user_config_path, &overlay, app_id, subs);
            let mut text = match reasoning_effort {
                Some(e) => format!("model switched to {model} (effort: {e})"),
                None => format!("model switched to {model}"),
            };
            if let Some(path) = saved {
                text.push_str(&format!("\nsaved to {}", path.display()));
            }
            emit(subs, AppEvent::Notify { app_id, text });
        }
        CommandOutcome::ProviderChanged { provider } => {
            apply_provider_change(provider, app_id, agent, config, user_config_path, subs).await;
        }
        CommandOutcome::ModeChanged { mode } => {
            agent.set_mode(mode);
            emit(subs, AppEvent::ModeChanged { app_id, mode });
        }
    }
    emit(subs, AppEvent::Idle { app_id });
}

async fn apply_provider_change(
    mut overlay: ProviderConfig,
    app_id: AppId,
    agent: &mut Agent,
    config: &mut AppConfig,
    user_config_path: Option<&Path>,
    subs: &Subscribers,
) {
    overlay.apply_name_presets();
    if overlay.reasoning_effort.is_none() && config.provider.reasoning_effort.is_none() {
        overlay.reasoning_effort = Some(ReasoningEffort::Medium);
    }
    let mut next = config.clone();
    next.merge(AppConfig {
        provider: overlay.clone(),
        ..AppConfig::default()
    });
    let model = next.provider.effective_model();
    match crate::provider::build_client(&next) {
        Ok(client) => {
            *config = next;
            agent
                .router_mut()
                .upsert(crate::provider::retrying(config, client));
            agent.set_model(model.clone());
            agent.set_reasoning_effort(config.provider.reasoning_effort);
            let saved = save_provider_overlay(user_config_path, &overlay, app_id, subs);
            emit(
                subs,
                AppEvent::ProviderUpdated {
                    app_id,
                    provider: ProviderConfig {
                        api_key: None,
                        ..config.provider.clone()
                    },
                },
            );
            emit_agent(
                subs,
                app_id,
                AgentEvent::ModelChanged {
                    agent_id: agent.id(),
                    model: model.clone(),
                    reasoning_effort: agent.reasoning_effort(),
                },
            );
            let (models, auth_error) = refresh_model_choices(agent.router(), &model, config).await;
            emit(subs, AppEvent::ModelsUpdated { app_id, models });
            emit(
                subs,
                AppEvent::Notify {
                    app_id,
                    text: summarize_setup(&overlay, saved.as_deref()),
                },
            );
            if let Some(body) = auth_error {
                emit(
                    subs,
                    AppEvent::Error {
                        app_id,
                        message: format!("API key rejected: {body}"),
                    },
                );
            }
        }
        Err(e) => {
            emit(
                subs,
                AppEvent::Error {
                    app_id,
                    message: e.to_string(),
                },
            );
        }
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

fn save_provider_overlay(
    path: Option<&Path>,
    overlay: &ProviderConfig,
    app_id: AppId,
    subs: &Subscribers,
) -> Option<PathBuf> {
    let path = path?;
    match AppConfig::save_provider_at(path, overlay) {
        Ok(()) => Some(path.to_path_buf()),
        Err(e) => {
            emit(
                subs,
                AppEvent::Error {
                    app_id,
                    message: e.to_string(),
                },
            );
            None
        }
    }
}

fn switch_session(store: &mut Option<SessionStore>, subs: &Subscribers, app_id: AppId) {
    if let Some(store) = store {
        let id = uuid::Uuid::now_v7().to_string();
        match Session::open(&store.dir, &id) {
            Ok(next) => store.set_current(next),
            Err(e) => {
                emit(
                    subs,
                    AppEvent::Error {
                        app_id,
                        message: e.to_string(),
                    },
                );
            }
        }
    }
}

/// Best-effort model list for the `/model` popup: static presets merged with
/// the provider's dynamic `/models` listing and the current model. A failed or
/// timed-out dynamic fetch silently falls back to the static data.
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
    (
        merge_model_choices(known, dynamic, current_model, &provider.provider_name()),
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
            out.push((id, provider_label(&provider)));
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

fn provider_label(name: &ProviderName) -> String {
    name.to_string()
}

/// Text of a user message: its text blocks joined with newlines. Empty when
/// the message carries no text (e.g. embedded tool results only).
fn user_message_text(m: &Message) -> String {
    m.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn runtime_loop(
    app_id: AppId,
    mut agent: Agent,
    mut session_store: Option<SessionStore>,
    mut cmd_rx: mpsc::UnboundedReceiver<AppCmd>,
    subscribers: Subscribers,
    mut config: AppConfig,
    user_config_path: Option<PathBuf>,
) {
    let model = agent.model().as_str();
    let live = agent.live_handle();
    let slash = SlashRegistry::with_builtin();
    // Leading in-memory messages already written to the store. On `/clear`
    // the in-memory history is replaced, so this resets to 0 while the store
    // itself is kept untouched; later turns append after the old content.
    // A brand-new session holds a meta record in memory but nothing on disk
    // yet, so its prefix starts at 0 and the first append writes the meta.
    let mut persisted_prefix = match &session_store {
        Some(store) if store.current().path().exists() => agent.history_records().len(),
        _ => 0,
    };
    let mut persisted_rev = agent.history_revision();

    // Publish the initial model list for the `/model` popup completion.
    let (models, _) = refresh_model_choices(agent.router(), model, &config).await;
    emit(&subscribers, AppEvent::ModelsUpdated { app_id, models });

    // Commands received while a turn is in flight are buffered here and run
    // in order once the current turn finishes, so UserInput is never dropped.
    let mut pending_cmds: VecDeque<AppCmd> = VecDeque::new();

    loop {
        let cmd = match pending_cmds.pop_front() {
            Some(cmd) => cmd,
            None => match cmd_rx.recv().await {
                Some(cmd) => cmd,
                None => break,
            },
        };
        match cmd {
            AppCmd::Shutdown => break,
            AppCmd::Cancel => {
                // no in-flight turn
            }
            AppCmd::SetMode(mode) => {
                agent.set_mode(mode);
                emit(&subscribers, AppEvent::ModeChanged { app_id, mode });
            }
            AppCmd::Rewind => {
                let removed = agent.rewind_last_turn();
                let text = removed.map(|m| user_message_text(&m));
                let found = TodoList::from_history(agent.history());
                let restored = found.clone().unwrap_or_default();
                agent.set_todos(restored.clone());
                let store_ok = match &session_store {
                    Some(store) => {
                        let mut recs = agent.history_records();
                        if found.is_some() {
                            recs.push(Record::TodoList {
                                timestamp: now_ms(),
                                items: restored.items.clone(),
                            });
                        }
                        match store.current().overwrite(&recs) {
                            Ok(()) => {
                                store.mark_content(agent.history().len() != 0);
                                true
                            }
                            Err(e) => {
                                emit(
                                    &subscribers,
                                    AppEvent::Error {
                                        app_id,
                                        message: e.to_string(),
                                    },
                                );
                                false
                            }
                        }
                    }
                    None => true,
                };
                if store_ok {
                    persisted_prefix = agent.history_records().len();
                }
                persisted_rev = agent.history_revision();
                emit_agent(
                    &subscribers,
                    app_id,
                    AgentEvent::TodoUpdated {
                        agent_id: agent.id(),
                        items: restored.items,
                    },
                );
                emit(
                    &subscribers,
                    AppEvent::Rewound {
                        app_id,
                        text,
                        messages: agent.history().cloned().collect(),
                        usage: *agent.total_usage(),
                    },
                );
            }
            AppCmd::UserInput(input) => {
                match slash.parse_and_run(&mut agent, &input) {
                    Ok(CommandOutcome::Passthrough) => {}
                    Ok(outcome) => {
                        apply_slash(
                            outcome,
                            app_id,
                            &mut agent,
                            &mut session_store,
                            &mut persisted_prefix,
                            &mut persisted_rev,
                            &subscribers,
                            &mut config,
                            user_config_path.as_deref(),
                        )
                        .await;
                        continue;
                    }
                    Err(e) => {
                        emit(
                            &subscribers,
                            AppEvent::Error {
                                app_id,
                                message: e.to_string(),
                            },
                        );
                        emit(&subscribers, AppEvent::Idle { app_id });
                        continue;
                    }
                }

                let cancel = CancellationToken::new();
                let (agent_tx, mut agent_rx) = mpsc::unbounded_channel();

                let result = {
                    let turn = agent.run_with_emitter(input, Some(agent_tx), Some(&cancel));
                    tokio::pin!(turn);

                    loop {
                        tokio::select! {
                            biased;
                            cmd = cmd_rx.recv() => {
                                match cmd {
                                    None | Some(AppCmd::Shutdown) => {
                                        cancel.cancel();
                                        let _ = turn.await;
                                        return;
                                    }
                                    Some(AppCmd::Cancel) => cancel.cancel(),
                                    Some(AppCmd::SetMode(mode)) => {
                                        apply_mode(&live, mode);
                                        emit(
                                            &subscribers,
                                            AppEvent::ModeChanged { app_id, mode },
                                        );
                                    }
                                    Some(AppCmd::UserInput(input)) => {
                                        pending_cmds.push_back(AppCmd::UserInput(input));
                                    }
                                    Some(AppCmd::Rewind) => {
                                        pending_cmds.push_back(AppCmd::Rewind);
                                    }
                                }
                            }
                            ev = agent_rx.recv() => {
                                match ev {
                                    Some(event) => {
                                        emit(&subscribers, AppEvent::Agent { app_id, event });
                                    }
                                    None => break turn.await,
                                }
                            }
                            res = &mut turn => break res,
                        }
                    }
                };

                if let Some(store) = &session_store {
                    agent.ensure_session_meta(store.root.clone());
                }

                while let Ok(event) = agent_rx.try_recv() {
                    emit(&subscribers, AppEvent::Agent { app_id, event });
                }

                match result {
                    Ok(_) => {
                        if let Some(store) = &session_store {
                            let rev = agent.history_revision();
                            let after = agent.history_records();
                            if rev != persisted_rev {
                                persisted_prefix = 0;
                                persisted_rev = rev;
                            } else if after.len() > persisted_prefix {
                                if let Err(e) =
                                    store.current().append_records(&after[persisted_prefix..])
                                {
                                    emit(
                                        &subscribers,
                                        AppEvent::Error {
                                            app_id,
                                            message: e.to_string(),
                                        },
                                    );
                                } else {
                                    store.mark_content(true);
                                    persisted_prefix = after.len();
                                    if let Err(e) = record_recent(
                                        &store.dir,
                                        Path::new(&store.root),
                                        store.current().id(),
                                    ) {
                                        emit(
                                            &subscribers,
                                            AppEvent::Error {
                                                app_id,
                                                message: e.to_string(),
                                            },
                                        );
                                    }
                                }
                            }
                            if should_persist_todos(&agent.todos(), agent.todo_written_this_turn())
                                && let Err(e) = persist_todo_snapshot(store, &agent.todos())
                            {
                                emit(
                                    &subscribers,
                                    AppEvent::Error {
                                        app_id,
                                        message: e.to_string(),
                                    },
                                );
                            }
                        }
                    }
                    Err(e) if e.is_cancelled() => {}
                    Err(e) => {
                        emit(
                            &subscribers,
                            AppEvent::Error {
                                app_id,
                                message: e.to_string(),
                            },
                        );
                    }
                }

                emit(&subscribers, AppEvent::Idle { app_id });
            }
        }
    }
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;

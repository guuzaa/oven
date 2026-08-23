use std::path::{Path, PathBuf};
use std::sync::Arc;

use oven_agent::AgentError;
use oven_agent::{Agent, Record, Skill, SkillReadTool, TodoWriteTool, Tool};
#[cfg(test)]
use oven_llm::Provider;
use oven_llm::{Role, Router};
use thiserror::Error;

use crate::config::{AppConfig, ConfigError};
use crate::dirs;
use crate::event::AppId;
use crate::handle::AppHandle;
use crate::instructions::{InstructionDoc, load_instructions};
use crate::mcp::McpRegistry;
use crate::mcp::client::{DefaultMcpConnector, McpConnector};
use crate::runtime::{hydrate_session, resolve_session, spawn_runtime};
use crate::session::{SessionError, canonical_root, default_sessions_dir};
use crate::{SkillRegistry, ToolRegistry};

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("app channel closed")]
    ChannelClosed,
    #[error("{0}")]
    Runtime(String),
    #[error("provider: {0}")]
    Provider(String),
    #[error("mcp: {0}")]
    Mcp(String),
}

impl From<oven_llm::ProviderError> for AppError {
    fn from(err: oven_llm::ProviderError) -> Self {
        AppError::Provider(err.to_string())
    }
}

/// App holds workspace context and resolves Provider + Agent runtime config
/// from the layered config system.
pub struct App {
    root: PathBuf,
    config: AppConfig,
    skills: SkillRegistry,
    tools: ToolRegistry,
    mcps: McpRegistry,
    instructions: Vec<InstructionDoc>,
    mcp_connector: Arc<dyn McpConnector>,
}

impl App {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            root: root.clone(),
            config: AppConfig::default(),
            skills: SkillRegistry::new(),
            tools: ToolRegistry::from_config(root, &[]),
            mcps: McpRegistry::new(),
            instructions: Vec::new(),
            mcp_connector: Arc::new(DefaultMcpConnector),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn skills(&self) -> &SkillRegistry {
        &self.skills
    }

    /// Register a skill module from code. Filesystem skills are discovered
    /// automatically when config is applied; this is for programmatic skills.
    /// Skills contribute system-prompt guidance only; they never mount tools
    /// (see [`crate::tools::ToolRegistry`]).
    pub fn register_skill(&mut self, skill: Box<dyn Skill>) {
        self.skills.register(skill);
    }

    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    pub fn mcps(&self) -> &McpRegistry {
        &self.mcps
    }

    /// Override how MCP servers are connected (used by tests).
    pub fn with_mcp_connector(mut self, connector: Arc<dyn McpConnector>) -> Self {
        self.mcp_connector = connector;
        self
    }

    /// Load config from the bundled default locations: user-level
    /// (`~/.oven/config.toml`, created as a template on first
    /// run) then project-level (`.oven.toml` in the workspace root). After
    /// loading, tools requested in `tools:` are mounted, MCP servers declared
    /// under `mcps:` are registered, and skills are discovered from the
    /// filesystem.
    pub fn load_config(&mut self) -> Result<(), AppError> {
        AppConfig::ensure_user_config()?;
        let user = AppConfig::default_user_config_path();
        let project = AppConfig::default_project_config_path(&self.root);
        let cfg = AppConfig::load(user.as_deref(), Some(&project))?;
        self.apply_config(cfg);
        Ok(())
    }

    /// Use an explicit, already-loaded config (e.g. for tests).
    pub fn with_config(mut self, config: AppConfig) -> Self {
        self.apply_config(config);
        self
    }

    fn apply_config(&mut self, mut config: AppConfig) {
        config.provider.normalize();
        self.tools = ToolRegistry::from_config(&self.root, &config.tools);
        self.mcps = McpRegistry::new();
        self.skills = SkillRegistry::new();
        self.skills.load_from_dirs(&dirs::skill_dirs(&self.root));
        self.instructions = load_instructions(dirs::config_home().as_deref(), &self.root);

        for (id, server) in &config.mcps {
            let _ = self.mcps.register(id.clone(), server.clone());
        }
        let sources = Arc::new(
            self.skills
                .sources()
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>(),
        );
        self.tools.register("read_skill", move || {
            Box::new(SkillReadTool::new(sources.clone()))
        });
        self.config = config;
    }

    fn build_router(&self) -> Result<Router, AppError> {
        crate::provider::build_router(&self.config)
    }

    pub(crate) async fn build_agent(&self) -> Result<Agent, AppError> {
        let model = self.config.provider.effective_model();
        let agent = self.build_agent_with_router(self.build_router()?).await?;
        Ok(agent.with_model(model))
    }

    pub(crate) async fn build_interactive_agent(&self) -> Result<Agent, AppError> {
        let model = self.config.provider.effective_model();
        let agent = self
            .build_agent_with_router(crate::provider::build_interactive_router(&self.config)?)
            .await?;
        Ok(agent.with_model(model))
    }

    #[cfg(test)]
    pub(crate) async fn build_agent_with_provider(
        &self,
        provider: Box<dyn Provider>,
    ) -> Result<Agent, AppError> {
        let mut router = Router::new();
        router.register(provider);
        self.build_agent_with_router(router).await
    }

    pub(crate) async fn build_agent_with_router(&self, router: Router) -> Result<Agent, AppError> {
        let mut tools = self.tools.merged_tools();
        let mcp_tools = self
            .mcp_connector
            .connect(&self.mcps, &self.root)
            .await
            .map_err(AppError::Mcp)?;
        tools.extend(mcp_tools.into_iter().map(|t| Box::new(t) as Box<dyn Tool>));
        tools.push(Box::new(TodoWriteTool));
        let mut agent = Agent::new(router, tools);
        agent.set_system(crate::system::system_prompt(
            &self.root,
            &self.instructions,
            self.skills.merged_system_prompt(),
        ));
        if let Some(effort) = self.config.provider.reasoning_effort {
            agent.set_reasoning_effort(Some(effort));
        }
        Ok(agent)
    }

    /// Run a single chat turn with no persistence (via the app runtime channel API).
    pub async fn query(&self, user: impl Into<String>) -> Result<String, AppError> {
        let handle = self.spawn().await?;
        let out = handle.prompt(user).await;
        handle.shutdown().await;
        out
    }

    /// Run a single chat turn inside a persisted session. On success the
    /// newly produced messages are appended to `<id>.jsonl` under the platform
    /// data dir. The session file is created lazily on first append.
    pub async fn run_session(
        &self,
        session_id: &str,
        user: impl Into<String>,
    ) -> Result<String, AppError> {
        let Some(dir) = default_sessions_dir() else {
            return Err(AppError::Session(SessionError::Io(
                PathBuf::from("<data_dir>"),
                std::io::Error::new(std::io::ErrorKind::NotFound, "no data_dir on this platform"),
            )));
        };
        self.run_session_in(&dir, session_id, user).await
    }

    /// Same as [`run_session`] but with an explicit sessions directory (used
    /// by tests and custom installs).
    pub async fn run_session_in(
        &self,
        sessions_dir: &Path,
        session_id: &str,
        user: impl Into<String>,
    ) -> Result<String, AppError> {
        let handle = self
            .spawn_session_in(sessions_dir, Some(session_id))
            .await?;
        let out = handle.prompt(user).await;
        handle.shutdown().await;
        out
    }

    /// Spawn a long-lived app task with no session persistence.
    pub async fn spawn(&self) -> Result<AppHandle, AppError> {
        let agent = self.build_agent().await?;
        Ok(spawn_runtime(
            AppId::next(),
            agent,
            None,
            self.root.clone(),
            self.config.clone(),
            AppConfig::default_user_config_path(),
        ))
    }

    /// Spawn with a persisted session under the platform data dir. `Some(id)`
    /// resumes that session when its file exists; otherwise (or for `None`) a
    /// new session is started with an auto-generated uuid v7 id that the
    /// caller never has to provide.
    pub async fn spawn_session(&self, session_id: Option<&str>) -> Result<AppHandle, AppError> {
        let Some(dir) = default_sessions_dir() else {
            let agent = self.build_interactive_agent().await?;
            return Ok(spawn_runtime(
                AppId::next(),
                agent,
                None,
                self.root.clone(),
                self.config.clone(),
                AppConfig::default_user_config_path(),
            ));
        };
        self.spawn_session_in(&dir, session_id).await
    }

    /// Same as [`App::spawn_session`] with an explicit sessions directory.
    pub(crate) async fn spawn_session_in(
        &self,
        sessions_dir: &Path,
        session_id: Option<&str>,
    ) -> Result<AppHandle, AppError> {
        let session = resolve_session(sessions_dir, session_id)?;
        let prior = session.load_records()?;
        let mut agent = self.build_interactive_agent().await?;
        let records: Vec<_> = prior
            .iter()
            .filter(
                |r| !matches!(r, Record::Message { message, .. } if message.role == Role::System),
            )
            .cloned()
            .collect();
        agent.restore_history(records);
        hydrate_session(&mut agent, &prior);
        agent.ensure_session_meta(canonical_root(&self.root));
        Ok(spawn_runtime(
            AppId::next(),
            agent,
            Some(session),
            self.root.clone(),
            self.config.clone(),
            AppConfig::default_user_config_path(),
        ))
    }
}

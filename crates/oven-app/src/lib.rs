use std::path::{Path, PathBuf};
use std::sync::Arc;

use oven_agent::{Agent, AgentError, SkillReadTool, Tool};
use oven_llm::Provider;
use thiserror::Error;

use crate::config::{AppConfig, ConfigError};
use crate::instructions::{InstructionDoc, default_config_home, load_instructions};
use crate::mcp::McpRegistry;
use crate::mcp::client::{DefaultMcpConnector, McpConnector};
use crate::session::{SessionError, default_sessions_dir};
use crate::skill::skill_dirs;

pub mod config;
mod instructions;
pub mod mcp;
mod provider;
pub mod runtime;
pub mod session;
pub mod skill;
mod slash;
pub mod tools;

pub use mcp::McpServerConfig;
pub use oven_agent::{AgentEvent, AgentId, CancellationToken, Skill, SkillRegistry};
pub use runtime::{AppCmd, AppEvent, AppHandle, AppId};
pub use tools::ToolRegistry;

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
    /// (`$XDG_CONFIG_HOME/oven/config.toml`, created as a template on first
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

    fn apply_config(&mut self, config: AppConfig) {
        self.tools = ToolRegistry::from_config(&self.root, &config.tools);
        self.mcps = McpRegistry::new();
        self.skills = SkillRegistry::new();
        self.skills.load_from_dirs(&skill_dirs(&self.root));
        self.instructions = load_instructions(default_config_home().as_deref(), &self.root);

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

    fn build_provider(&self, model: &str) -> Result<Box<dyn Provider>, AppError> {
        crate::provider::build_provider(&self.config, model)
    }

    fn build_system_prompt(&self) -> String {
        let mut base =
            String::from("You are a coding assistant working inside the user's repository.");
        for doc in &self.instructions {
            base.push_str(&format!(
                "\n\n## {} Instructions (from {})\n\n{}\n\n",
                doc.scope,
                doc.path.display(),
                doc.content
            ));
        }
        if let Some(extra) = self.skills.merged_system_prompt() {
            base.push_str(&format!("## Available Skills\n\n{}", extra));
        }
        base
    }

    pub(crate) async fn build_agent(&self) -> Result<Agent, AppError> {
        let model = self.config.provider.effective_model();
        let agent = self
            .build_agent_with_provider(self.build_provider(&model)?)
            .await?;
        Ok(agent.with_model(model))
    }

    pub(crate) async fn build_interactive_agent(&self) -> Result<Agent, AppError> {
        let model = self.config.provider.effective_model();
        let agent = self
            .build_agent_with_provider(crate::provider::build_interactive_provider(
                &self.config,
                &model,
            )?)
            .await?;
        Ok(agent.with_model(model))
    }

    pub(crate) async fn build_agent_with_provider(
        &self,
        provider: Box<dyn Provider>,
    ) -> Result<Agent, AppError> {
        let mut tools = self.tools.merged_tools();
        let mcp_tools = self
            .mcp_connector
            .connect(&self.mcps, &self.root)
            .await
            .map_err(AppError::Mcp)?;
        tools.extend(mcp_tools.into_iter().map(|t| Box::new(t) as Box<dyn Tool>));
        Ok(Agent::new(provider, tools).with_system(self.build_system_prompt()))
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::InstructionScope;

    #[test]
    fn system_prompt_includes_instruction_docs() {
        let tmp = tempdir::TempDir::new("app-instructions").unwrap();
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "project rules\n").unwrap();

        let app = App::new(&root).with_config(AppConfig::default());
        let prompt = app.build_system_prompt();
        assert!(prompt.contains("## Project Instructions"));
        assert!(prompt.contains("project rules"));
    }

    #[test]
    fn system_prompt_labels_user_and_project_docs() {
        let mut app = App::new(".");
        app.instructions = vec![
            InstructionDoc {
                scope: InstructionScope::Global,
                path: PathBuf::from("/cfg/AGENTS.md"),
                content: "global rules\n".into(),
            },
            InstructionDoc {
                scope: InstructionScope::Project,
                path: PathBuf::from("/ws/CLAUDE.md"),
                content: "project rules\n".into(),
            },
        ];
        let prompt = app.build_system_prompt();
        assert!(prompt.contains("## Global Instructions (from /cfg/AGENTS.md)"));
        assert!(prompt.contains("## Project Instructions (from /ws/CLAUDE.md)"));
        assert!(prompt.find("global rules").unwrap() < prompt.find("project rules").unwrap());
    }
}

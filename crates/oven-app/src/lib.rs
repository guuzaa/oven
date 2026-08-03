use std::env;
use std::path::{Path, PathBuf};

use oven_agent::{Agent, AgentError, RetryingProvider};
use oven_llm::{CompletionsProvider, Provider, ProviderName};
use thiserror::Error;

use crate::config::{AppConfig, ConfigError};
use crate::mcp::McpRegistry;
use crate::session::{SessionError, default_sessions_dir};
use crate::skill::SkillRegistry;

pub mod config;
pub mod mcp;
pub mod runtime;
pub mod session;
pub mod skill;
pub mod tools;

pub use mcp::McpServerConfig;
pub use oven_agent::{AgentEvent, AgentId, CancellationToken};
pub use runtime::{AppCmd, AppEvent, AppHandle, AppId};
pub use skill::Skill;
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
}

/// App holds workspace context and resolves Provider + Agent runtime config
/// from the layered config system.
pub struct App {
    root: PathBuf,
    config: AppConfig,
    skills: SkillRegistry,
    tools: ToolRegistry,
    mcps: McpRegistry,
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

    /// Register a skill module. Skills contribute system-prompt guidance
    /// only; they never mount tools (see [`crate::tools::ToolRegistry`]).
    pub fn register_skill(&mut self, skill: Box<dyn Skill>) {
        self.skills.register(skill);
    }

    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    pub fn mcps(&self) -> &McpRegistry {
        &self.mcps
    }

    /// Load config from the bundled default locations: user-level
    /// (`~/.config/oven/config.yaml`) then project-level (`.oven.yaml` in the
    /// workspace root). After loading, skills requested in `skills:` and tools
    /// requested in `tools:` are registered, and MCP servers declared under
    /// `mcps:` are registered.
    pub fn load_config(&mut self) -> Result<(), AppError> {
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
        self.skills = SkillRegistry::new();
        self.tools = ToolRegistry::from_config(&self.root, &config.tools);
        self.mcps = McpRegistry::new();

        for (id, server) in &config.mcps {
            let _ = self.mcps.register(id.clone(), server.clone());
        }
        self.config = config;
    }

    fn effective_model(&self) -> String {
        if let Some(m) = &self.config.provider.model {
            return m.clone();
        }
        if let Ok(m) = env::var("OVEN_MODEL") {
            return m;
        }
        "gpt-4o-mini".to_string()
    }

    fn effective_base_url(&self, model: &str) -> Option<String> {
        if let Some(u) = &self.config.provider.base_url {
            return Some(u.clone());
        }
        if let Ok(u) = env::var("OVEN_BASE_URL")
            && !u.is_empty()
        {
            return Some(u);
        }
        let lower = model.to_ascii_lowercase();
        if lower.starts_with("claude") {
            env::var("ANTHROPIC_BASE_URL")
                .ok()
                .filter(|v| !v.is_empty())
        } else if lower.starts_with("gpt") || lower.starts_with("o1") || lower.starts_with("o3") {
            env::var("OPENAI_BASE_URL").ok().filter(|v| !v.is_empty())
        } else {
            None
        }
    }

    fn effective_api_key(&self, model: &str) -> String {
        if let Some(k) = &self.config.provider.api_key {
            return k.clone();
        }
        let lower = model.to_ascii_lowercase();
        if lower.starts_with("claude") {
            env::var("ANTHROPIC_API_KEY").unwrap_or_default()
        } else if lower.starts_with("gpt") || lower.starts_with("o1") || lower.starts_with("o3") {
            env::var("OPENAI_API_KEY").unwrap_or_default()
        } else if lower.starts_with("deepseek") {
            env::var("DEEPSEEK_API_KEY").unwrap_or_default()
        } else if lower.starts_with("kimi") {
            env::var("MOONSHOT_API_KEY").unwrap_or_default()
        } else if lower.starts_with("glm") {
            env::var("OVEN_ZHIPU_API_KEY").unwrap_or_default()
        } else {
            env::var("OPENAI_API_KEY").unwrap_or_default()
        }
    }

    fn determine_provider_name(model: &str) -> ProviderName {
        let lower = model.to_ascii_lowercase();
        if lower.starts_with("claude") {
            ProviderName::Anthropic
        } else if lower.starts_with("gpt") || lower.starts_with("o1") || lower.starts_with("o3") {
            ProviderName::OpenAI
        } else if lower.starts_with("deepseek") {
            ProviderName::DeepSeek
        } else if lower.starts_with("kimi") {
            ProviderName::Moonshot
        } else if lower.starts_with("glm") {
            ProviderName::Zhipu
        } else {
            ProviderName::Custom("unknown".into())
        }
    }

    fn build_provider(&self, model: &str) -> Result<Box<dyn Provider>, AppError> {
        let provider_name = Self::determine_provider_name(model);
        let api_key = self.effective_api_key(model);
        let base_url = self.effective_base_url(model);

        // Anthropic and unknown models have no chat-completions preset; without
        // an explicit OpenAI-compatible base URL there is no endpoint to hit.
        if base_url.is_none() {
            match &provider_name {
                ProviderName::Anthropic => {
                    return Err(AppError::Provider(format!(
                        "model '{model}' needs an OpenAI-compatible proxy; set ANTHROPIC_BASE_URL or provider.base_url"
                    )));
                }
                ProviderName::Custom(_) => {
                    return Err(AppError::Provider(format!(
                        "unknown provider for model '{model}'; set provider.base_url or OVEN_BASE_URL to use an OpenAI-compatible endpoint"
                    )));
                }
                _ => {}
            }
        }
        if base_url.is_none() && api_key.is_empty() {
            return Err(AppError::Provider(format!(
                "no API key for model '{model}'; set the matching API key env var or provider.api_key"
            )));
        }

        let provider = match &base_url {
            Some(u) => CompletionsProvider::with_base_url(u.clone(), provider_name, api_key),
            None => CompletionsProvider::new(provider_name, api_key),
        };
        let retrying = RetryingProvider::new(Box::new(provider))
            .with_timeout(self.config.request_timeout())
            .with_retries(self.config.max_retries)
            .with_base_backoff(self.config.base_backoff());
        Ok(Box::new(retrying))
    }

    fn build_system_prompt(&self) -> String {
        let mut base =
            String::from("You are a coding assistant working inside the user's repository.");
        if let Some(extra) = self.skills.merged_system_prompt() {
            base.push_str("\n\n");
            base.push_str(&extra);
        }
        base
    }

    pub(crate) fn build_agent(&self) -> Result<Agent, AppError> {
        let model = self.effective_model();
        Ok(self
            .build_agent_with_provider(self.build_provider(&model)?)
            .with_model(model))
    }

    pub(crate) fn build_agent_with_provider(&self, provider: Box<dyn Provider>) -> Agent {
        let tools = self.tools.merged_tools();
        Agent::new(provider, tools).with_system(self.build_system_prompt())
    }

    /// Run a single chat turn with no persistence (via the app runtime channel API).
    pub async fn run_chat(&self, user: impl Into<String>) -> Result<String, AppError> {
        let handle = self.spawn()?;
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
        let handle = self.spawn_session_in(sessions_dir, session_id)?;
        let out = handle.prompt(user).await;
        handle.shutdown().await;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::App;
    use oven_llm::ProviderName;

    #[test]
    fn provider_routes_model_prefixes() {
        let cases = [
            ("gpt-4o", ProviderName::OpenAI),
            ("o3-mini", ProviderName::OpenAI),
            ("deepseek-chat", ProviderName::DeepSeek),
            ("kimi-k2", ProviderName::Moonshot),
            ("glm-4", ProviderName::Zhipu),
            ("claude-3-5-haiku", ProviderName::Anthropic),
        ];
        for (model, expected) in cases {
            assert_eq!(App::determine_provider_name(model), expected, "{model}");
        }
    }
}

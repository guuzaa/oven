use std::path::{Path, PathBuf};
use std::sync::Arc;

use oven_agent::{
    Agent, AgentId, AnswerTool, InstructionDoc, Record, RoleSpec, Skill, SkillReadTool,
    SubagentSpawner, TaskOutputTool, TaskTool, TodoWriteTool, Tool, ToolPermission,
    load_instructions, router_handle, subagent_preamble,
};
#[cfg(test)]
use oven_llm::Provider;
use oven_llm::{Role, Router};
use tokio::sync::mpsc;
use tracing::Instrument;

use crate::App;
use crate::AppError;
use crate::core::config::AppConfig;
use crate::core::config::ProviderConfig;
use crate::core::event::{AppId, EventBus};
use crate::core::session::{Session, canonical_root, session_span};
use crate::dirs;
use crate::mcp::McpRegistry;
use crate::mcp::client::{DefaultMcpConnector, McpConnector};
use crate::runtime::{AppAgents, hydrate_session, spawn_runtime};
use crate::subagent::{Role as SubagentRole, SubagentParts, Subagents};
use crate::{SkillRegistry, ToolRegistry};

/// Tools a subagent never mounts: the ones that speak to the user, manage
/// the caller's plan, or would nest delegation.
const CHILD_TOOLS_EXCLUDED: &[&str] = &[
    AnswerTool::NAME,
    TodoWriteTool::NAME,
    TaskTool::NAME,
    TaskOutputTool::NAME,
];

const EXPLORE_ROLE: &str = "explore";
const GENERAL_ROLE: &str = "general";
const EXPLORE_GUIDANCE: &str = "You may only read: search, read files, walk directories. You cannot \
write, edit or run anything, so report what should change instead of changing it. Name the files \
and line numbers you found.";
const GENERAL_GUIDANCE: &str = "You have the full tool set. Nobody can answer questions while you \
work, so make the reasonable assumption, state it in your answer, and carry on.";

fn role_system(system: &str, role: &str, guidance: &str) -> String {
    format!("{system}\n\n{}", subagent_preamble(role, guidance))
}

pub struct AppBuilder {
    root: PathBuf,
    config: AppConfig,
    skills: SkillRegistry,
    tools: ToolRegistry,
    mcps: McpRegistry,
    instructions: Vec<InstructionDoc>,
    mcp_connector: Arc<dyn McpConnector>,
}

impl AppBuilder {
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

    fn apply_config(&mut self, config: AppConfig) {
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
        crate::core::provider::build_router(&self.config)
    }

    pub(crate) async fn build_agent(&self) -> Result<AppAgents, AppError> {
        let model = self.active_model()?;
        let mut agents = self.build_agent_with_router(self.build_router()?).await?;
        agents.main.set_model(model);
        Ok(agents)
    }

    pub(crate) async fn build_interactive_agent(&self) -> Result<AppAgents, AppError> {
        let model = self.active_model()?;
        let mut agents = self
            .build_agent_with_router(crate::core::provider::build_interactive_router(
                &self.config,
            )?)
            .await?;
        agents.main.set_model(model);
        Ok(agents)
    }

    fn active_model(&self) -> Result<String, AppError> {
        self.config
            .active_provider_config()
            .map(ProviderConfig::effective_model)
            .ok_or_else(|| AppError::Provider("no active provider configured".into()))
    }

    #[cfg(test)]
    pub(crate) async fn build_agent_with_provider(
        &self,
        provider: Box<dyn Provider>,
    ) -> Result<AppAgents, AppError> {
        let mut router = Router::new();
        router.register(provider);
        self.build_agent_with_router(router).await
    }

    /// Compose one app's agents: the conversation driver, the subagents it
    /// may spawn, and the bus they report on. Building them together is what
    /// lets a subagent share the driver's router and publish while the
    /// runtime is busy with a turn of its own.
    pub(crate) async fn build_agent_with_router(
        &self,
        router: Router,
    ) -> Result<AppAgents, AppError> {
        let mut base = self.tools.merged_tools();
        let mcp_tools = self
            .mcp_connector
            .connect(&self.mcps, &self.root)
            .await
            .map_err(AppError::Mcp)?;
        base.extend(mcp_tools.into_iter().map(|t| Arc::new(t) as Arc<dyn Tool>));

        let system = oven_agent::system_prompt(
            &self.root,
            &self.instructions,
            self.skills.merged_system_prompt(),
        );
        let events = EventBus::new();
        let (wake, wake_rx) = mpsc::unbounded_channel();
        // `wake` goes to the supervisor, which keeps the channel open; the
        // runtime only ever listens on it.
        let router = router_handle(router);
        let main_id = AgentId::next();
        let subagents = Subagents::new(SubagentParts {
            parent: main_id,
            router: router.clone(),
            roles: Self::build_roles(&base, &system),
            max_concurrent: self.config.subagents.max_concurrent,
            max_iters: self.config.subagents.max_iters,
            events: events.clone(),
            wake: wake.clone(),
        });

        let mut tools = base;
        if self.config.subagents.enabled {
            let spawner: Arc<dyn SubagentSpawner> = subagents.clone();
            tools.push(Arc::new(TaskTool::new(
                spawner.clone(),
                subagents.role_specs(),
            )));
            tools.push(Arc::new(TaskOutputTool::new(spawner)));
        }
        let mut main = Agent::with_router(router, tools)
            .with_id(main_id)
            .with_system(system);
        if let Some(effort) = self
            .config
            .active_provider_config()
            .and_then(|provider| provider.reasoning_effort)
        {
            main.set_reasoning_effort(Some(effort));
        }
        drop(wake);
        Ok(AppAgents {
            main,
            subagents,
            events,
            wake_rx,
        })
    }

    /// The roles a subagent may run as. Tools are partitioned by what they
    /// may do rather than listed by hand, so a tool that joins the read-only
    /// set reaches `explore` without touching this.
    fn build_roles(base: &[Arc<dyn Tool>], system: &str) -> Vec<SubagentRole> {
        let child_tools = |keep: fn(&Arc<dyn Tool>) -> bool| -> Vec<Arc<dyn Tool>> {
            base.iter()
                .filter(|tool| !CHILD_TOOLS_EXCLUDED.contains(&tool.name()) && keep(tool))
                .cloned()
                .collect()
        };
        vec![
            SubagentRole::new(
                RoleSpec {
                    name: EXPLORE_ROLE.into(),
                    description: "read-only search and reading; it cannot change anything".into(),
                    read_only: true,
                },
                child_tools(|tool| tool.caps().permission == ToolPermission::Read),
                role_system(system, EXPLORE_ROLE, EXPLORE_GUIDANCE),
            ),
            SubagentRole::new(
                RoleSpec {
                    name: GENERAL_ROLE.into(),
                    description: "every tool, including writes and shell commands".into(),
                    read_only: false,
                },
                child_tools(|_| true),
                role_system(system, GENERAL_ROLE, GENERAL_GUIDANCE),
            ),
        ]
    }

    /// Start a long-lived app task with no session persistence.
    pub async fn open(&self) -> Result<App, AppError> {
        let agents = self.build_agent().await?;
        self.log_open(&agents.main);
        Ok(spawn_runtime(
            AppId::next(),
            agents,
            None,
            self.root.clone(),
            self.config.clone(),
            AppConfig::default_user_config_path(),
        ))
    }

    /// Start with a persisted session under the platform data dir. `Some(id)`
    /// resumes that session when its file exists; otherwise (or for `None`) a
    /// new session is started with an auto-generated uuid v7 id that the
    /// caller never has to provide.
    pub async fn open_session(&self, session_id: Option<&str>) -> Result<App, AppError> {
        let Some(dir) = dirs::sessions_dir() else {
            let agents = self.build_interactive_agent().await?;
            self.log_open(&agents.main);
            return Ok(spawn_runtime(
                AppId::next(),
                agents,
                None,
                self.root.clone(),
                self.config.clone(),
                AppConfig::default_user_config_path(),
            ));
        };
        self.open_session_in(&dir, session_id).await
    }

    /// Same as [`AppBuilder::open_session`] with an explicit sessions directory.
    pub(crate) async fn open_session_in(
        &self,
        sessions_dir: &Path,
        session_id: Option<&str>,
    ) -> Result<App, AppError> {
        let session = Session::resolve(sessions_dir, session_id)?;
        let span = session_span(Some(session.id()));
        async {
            let prior = session.load_records()?;
            let mut agents = self.build_interactive_agent().await?;
            let records: Vec<_> = prior
                .iter()
                .filter(
                    |r| !matches!(r, Record::Message { message, .. } if message.role == Role::System),
                )
                .cloned()
                .collect();
            agents.main.restore_history(records);
            hydrate_session(&mut agents.main, &prior);
            agents.main.ensure_session_meta(canonical_root(&self.root));
            self.log_open(&agents.main);
            Ok(spawn_runtime(
                AppId::next(),
                agents,
                Some(session),
                self.root.clone(),
                self.config.clone(),
                AppConfig::default_user_config_path(),
            ))
        }
        .instrument(span)
        .await
    }

    fn log_open(&self, agent: &Agent) {
        tracing::info!(
            root = %self.root.display(),
            model = %agent.model(),
            tool_count = self.tools.len(),
            mcp_count = self.mcps.len(),
            "app opened"
        );
    }
}

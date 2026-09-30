mod agents;
mod clear;
mod compact;
mod exit;
mod model;
mod plan;
mod setup;

use oven_agent::{Agent, AgentId, AgentMode};

use crate::core::config::ProviderConfig;
use crate::core::error::AppError;
use crate::subagent::Subagents;

pub use agents::Agents;
pub use clear::Clear;
pub use compact::Compact;
pub use exit::Exit;
pub(crate) use model::{Model, ModelDirective};
pub use plan::Plan;
pub use setup::Setup;

/// What a command may reach: the conversation driver it configures, and the
/// subagents it may list or stop. Commands stay read-mostly — anything that
/// changes app state comes back as a [`CommandOutcome`] the runtime applies —
/// but the subagent registry is shared state a command may act on directly.
///
/// The driver is optional because a running turn holds it exclusively. A
/// command that asks for it mid-turn is deferred rather than failed, which is
/// what keeps `/clear` and `/setup` off a turn in flight while letting
/// `/agents` — which only ever touches the registry — apply immediately.
pub struct CommandContext<'a> {
    agent: Option<&'a mut Agent>,
    pub subagents: &'a Subagents,
}

impl<'a> CommandContext<'a> {
    /// A command running with the driver free to use.
    pub fn with_agent(agent: &'a mut Agent, subagents: &'a Subagents) -> Self {
        Self {
            agent: Some(agent),
            subagents,
        }
    }

    /// A command running while a turn holds the driver.
    pub fn shared(subagents: &'a Subagents) -> Self {
        Self {
            agent: None,
            subagents,
        }
    }

    /// The driver, or [`AppError::AgentBusy`] when a turn holds it.
    pub fn agent(&mut self) -> Result<&mut Agent, AppError> {
        self.agent.as_deref_mut().ok_or(AppError::AgentBusy)
    }
}

#[derive(Debug, Clone)]
pub enum CommandOutcome {
    Reply(String),
    Cleared,
    Compact,
    Exit,
    ModelChanged {
        model: String,
        reasoning_effort: Option<oven_llm::ReasoningEffort>,
    },
    ProviderChanged {
        provider: ProviderConfig,
    },
    ModeChanged {
        mode: AgentMode,
    },
    FocusSubagent {
        id: AgentId,
    },
    Passthrough,
}

pub trait SlashCommand: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn execute(&self, cx: &mut CommandContext<'_>, args: &str) -> Result<CommandOutcome, AppError>;
}

pub struct SlashRegistry {
    commands: Vec<Box<dyn SlashCommand>>,
}

impl SlashRegistry {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    pub fn with_builtin() -> Self {
        let mut r = Self::new();
        r.register(Box::new(Clear));
        r.register(Box::new(Compact));
        r.register(Box::new(Exit));
        r.register(Box::new(Model));
        r.register(Box::new(Setup));
        r.register(Box::new(Plan));
        r.register(Box::new(Agents));
        r
    }

    pub fn register(&mut self, cmd: Box<dyn SlashCommand>) {
        self.commands.push(cmd);
    }

    /// (name, description) pairs for every registered command, in
    /// registration order.
    pub fn commands(&self) -> Vec<(String, String)> {
        self.commands
            .iter()
            .map(|c| (c.name().to_string(), c.description().to_string()))
            .collect()
    }

    /// The registered command `input` invokes and its trimmed arguments, or
    /// `None` when `input` is not a slash invocation or names an unknown
    /// command. The one parser every entry point shares, so the registry
    /// cannot disagree with itself about what counts as a command.
    pub fn invocation<'a>(&self, input: &'a str) -> Option<(&'a str, &'a str)> {
        let body = input.trim_start().strip_prefix('/')?;
        let (name, args) = match body.split_once(char::is_whitespace) {
            Some((name, rest)) => (name, rest.trim()),
            None => (body, ""),
        };
        self.commands
            .iter()
            .any(|c| c.name() == name)
            .then_some((name, args))
    }

    pub fn run(
        &self,
        cx: &mut CommandContext<'_>,
        name: &str,
        args: &str,
    ) -> Result<CommandOutcome, AppError> {
        match self.commands.iter().find(|c| c.name() == name) {
            Some(command) => command.execute(cx, args),
            None => Ok(CommandOutcome::Passthrough),
        }
    }

    /// Runs a command with no driver, which succeeds only for commands that
    /// never ask for one. `Ok(None)` means the command needs the driver, so
    /// the runtime should defer it until the running turn ends.
    pub fn run_shared(
        &self,
        subagents: &Subagents,
        name: &str,
        args: &str,
    ) -> Result<Option<CommandOutcome>, AppError> {
        let mut cx = CommandContext::shared(subagents);
        match self.run(&mut cx, name, args) {
            Ok(CommandOutcome::Passthrough) | Err(AppError::AgentBusy) => Ok(None),
            Ok(outcome) => Ok(Some(outcome)),
            Err(error) => Err(error),
        }
    }
}

impl Default for SlashRegistry {
    fn default() -> Self {
        Self::with_builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagent::Subagents;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use oven_llm::{
        Message, ModelId, ModelInfo, Provider, ProviderError, ProviderName, Request, Response,
        Result as LlmResult, Router, StreamEvent,
    };

    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        async fn complete(&self, _req: &Request) -> LlmResult<Response> {
            Err(ProviderError::Api {
                status: 500,
                body: "unused".into(),
            })
        }

        async fn stream(
            &self,
            _req: &Request,
        ) -> LlmResult<BoxStream<'static, LlmResult<StreamEvent>>> {
            Err(ProviderError::Api {
                status: 500,
                body: "unused".into(),
            })
        }

        fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
            None
        }

        fn provider_name(&self) -> ProviderName {
            ProviderName::Custom("mock".into())
        }
    }

    fn fresh_agent() -> Agent {
        let mut router = Router::new();
        router.register(Box::new(MockProvider));
        Agent::new(router, Vec::new())
    }

    fn with_context<T>(run: impl FnOnce(&mut CommandContext<'_>) -> T) -> T {
        let mut agent = fresh_agent();
        let subagents = Subagents::bare(agent.id(), agent.router_handle());
        let mut cx = CommandContext::with_agent(&mut agent, &subagents);
        run(&mut cx)
    }

    #[test]
    fn passthrough_when_unknown_command() {
        let reg = SlashRegistry::with_builtin();
        let outcome = with_context(|cx| reg.run(cx, "nope", "")).unwrap();
        assert!(matches!(outcome, CommandOutcome::Passthrough));
    }

    #[test]
    fn invocation_splits_name_from_trimmed_args() {
        let reg = SlashRegistry::with_builtin();
        assert_eq!(reg.invocation("/model gpt-4o"), Some(("model", "gpt-4o")));
        assert_eq!(reg.invocation("  /plan   on  "), Some(("plan", "on")));
        assert_eq!(reg.invocation("/model"), Some(("model", "")));
    }

    #[test]
    fn invocation_recognizes_only_registered_commands() {
        let reg = SlashRegistry::with_builtin();
        for text in [
            "/clear",
            "/compact",
            "/exit",
            "/model",
            "/model gpt-4o high",
            "/setup name=deepseek api_key=sk-secret",
            "/plan on",
            "  /plan",
            "/agents",
            "/agents stop all",
        ] {
            assert!(
                reg.invocation(text).is_some(),
                "{text} must be a control command"
            );
        }
        for text in ["hello there", "/nope", "! ls", " /nope args"] {
            assert!(
                reg.invocation(text).is_none(),
                "{text} must reach the agent"
            );
        }
    }

    #[test]
    fn commands_returns_names_and_descriptions() {
        let reg = SlashRegistry::with_builtin();
        let cmds = reg.commands();
        assert_eq!(cmds.len(), 7);
        assert!(cmds.iter().any(|(n, d)| n == "clear" && !d.is_empty()));
        assert!(cmds.iter().any(|(n, d)| n == "compact" && !d.is_empty()));
        assert!(cmds.iter().any(|(n, _)| n == "exit"));
        assert!(cmds.iter().any(|(n, d)| n == "model" && !d.is_empty()));
        assert!(cmds.iter().any(|(n, d)| n == "setup" && !d.is_empty()));
        assert!(cmds.iter().any(|(n, d)| n == "plan" && !d.is_empty()));
        assert!(cmds.iter().any(|(n, d)| n == "agents" && !d.is_empty()));
    }

    #[test]
    fn clear_wipes_history() {
        let reg = SlashRegistry::with_builtin();
        let mut agent = fresh_agent();
        agent.push_history(Message::user_text("hi"));
        agent.set_todos(oven_agent::TodoList {
            items: vec![oven_agent::TodoItem {
                id: "a".into(),
                content: "one".into(),
                status: oven_agent::TodoStatus::Pending,
            }],
        });
        let subagents = Subagents::bare(agent.id(), agent.router_handle());
        let outcome = reg
            .run(
                &mut CommandContext::with_agent(&mut agent, &subagents),
                "clear",
                "",
            )
            .unwrap();
        assert!(matches!(outcome, CommandOutcome::Cleared));
        assert_eq!(agent.history().len(), 0);
        assert!(agent.todos().is_empty());
    }

    #[test]
    fn exit_returns_exit_outcome() {
        let reg = SlashRegistry::with_builtin();
        let outcome = with_context(|cx| reg.run(cx, "exit", "")).unwrap();
        assert!(matches!(outcome, CommandOutcome::Exit));
    }

    #[test]
    fn args_are_parsed_after_command_name() {
        struct Echo;
        impl SlashCommand for Echo {
            fn name(&self) -> &str {
                "echo"
            }
            fn description(&self) -> &str {
                ""
            }
            fn execute(
                &self,
                _cx: &mut CommandContext<'_>,
                args: &str,
            ) -> Result<CommandOutcome, AppError> {
                Ok(CommandOutcome::Reply(args.to_string()))
            }
        }
        let mut reg = SlashRegistry::new();
        reg.register(Box::new(Echo));
        let out = with_context(|cx| reg.run(cx, "echo", "hello world")).unwrap();
        match out {
            CommandOutcome::Reply(s) => assert_eq!(s, "hello world"),
            _ => panic!("expected Reply"),
        }
    }
}

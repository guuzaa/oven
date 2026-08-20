mod clear;
mod exit;
mod model;
mod plan;
mod setup;

use oven_agent::{Agent, AgentMode};

use crate::AppError;
use crate::config::ProviderConfig;

pub use clear::Clear;
pub use exit::Exit;
pub use model::Model;
pub use plan::Plan;
pub use setup::Setup;

#[derive(Debug, Clone)]
pub enum CommandOutcome {
    Reply(String),
    Cleared,
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
    Passthrough,
}

pub trait SlashCommand: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn execute(&self, agent: &mut Agent, args: &str) -> Result<CommandOutcome, AppError>;
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
        r.register(Box::new(Exit));
        r.register(Box::new(Model));
        r.register(Box::new(Setup));
        r.register(Box::new(Plan));
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

    pub fn parse_and_run(
        &self,
        agent: &mut Agent,
        input: &str,
    ) -> Result<CommandOutcome, AppError> {
        let trimmed = input.trim_start();
        if !trimmed.starts_with('/') {
            return Ok(CommandOutcome::Passthrough);
        }
        let body = &trimmed[1..];
        let (name, args) = match body.split_once(char::is_whitespace) {
            Some((n, rest)) => (n, rest.trim()),
            None => (body, ""),
        };
        let command = self
            .commands
            .iter()
            .find(|c| c.name() == name)
            .ok_or_else(|| AppError::Runtime(format!("unknown command: /{name}")))?;
        command.execute(agent, args)
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

    #[test]
    fn passthrough_when_not_slash() {
        let reg = SlashRegistry::with_builtin();
        let mut agent = fresh_agent();
        let outcome = reg.parse_and_run(&mut agent, "hi there").unwrap();
        assert!(matches!(outcome, CommandOutcome::Passthrough));
    }

    #[test]
    fn unknown_command_errors() {
        let reg = SlashRegistry::with_builtin();
        let mut agent = fresh_agent();
        let err = reg.parse_and_run(&mut agent, "/nope").unwrap_err();
        assert!(err.to_string().contains("unknown command"));
    }

    #[test]
    fn commands_returns_names_and_descriptions() {
        let reg = SlashRegistry::with_builtin();
        let cmds = reg.commands();
        assert_eq!(cmds.len(), 5);
        assert!(cmds.iter().any(|(n, d)| n == "clear" && !d.is_empty()));
        assert!(cmds.iter().any(|(n, _)| n == "exit"));
        assert!(cmds.iter().any(|(n, d)| n == "model" && !d.is_empty()));
        assert!(cmds.iter().any(|(n, d)| n == "setup" && !d.is_empty()));
        assert!(cmds.iter().any(|(n, d)| n == "plan" && !d.is_empty()));
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
        let outcome = reg.parse_and_run(&mut agent, "/clear").unwrap();
        assert!(matches!(outcome, CommandOutcome::Cleared));
        assert_eq!(agent.history().len(), 0);
        assert!(agent.todos().is_empty());
    }

    #[test]
    fn exit_returns_exit_outcome() {
        let reg = SlashRegistry::with_builtin();
        let mut agent = fresh_agent();
        let outcome = reg.parse_and_run(&mut agent, "/exit").unwrap();
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
            fn execute(&self, _a: &mut Agent, args: &str) -> Result<CommandOutcome, AppError> {
                Ok(CommandOutcome::Reply(args.to_string()))
            }
        }
        let mut reg = SlashRegistry::new();
        reg.register(Box::new(Echo));
        let mut agent = fresh_agent();
        let out = reg
            .parse_and_run(&mut agent, "/echo   hello world")
            .unwrap();
        match out {
            CommandOutcome::Reply(s) => assert_eq!(s, "hello world"),
            _ => panic!("expected Reply"),
        }
    }
}

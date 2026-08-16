use oven_agent::{Agent, AgentMode};

use super::{CommandOutcome, SlashCommand};
use crate::AppError;

pub struct Plan;

impl SlashCommand for Plan {
    fn name(&self) -> &str {
        "plan"
    }

    fn description(&self) -> &str {
        "Switch plan mode: /plan [on|off]"
    }

    fn execute(&self, agent: &mut Agent, args: &str) -> Result<CommandOutcome, AppError> {
        match args.trim().to_ascii_lowercase().as_str() {
            "" => Ok(CommandOutcome::Reply(format!(
                "current mode: {}\n{}",
                agent.mode().label(),
                agent.todos().summary()
            ))),
            "on" => Ok(CommandOutcome::ModeChanged {
                mode: AgentMode::Plan,
            }),
            "off" => Ok(CommandOutcome::ModeChanged {
                mode: AgentMode::Default,
            }),
            _ => Err(AppError::Runtime("usage: /plan [on|off]".to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use oven_llm::{
        ModelId, ModelInfo, Provider, ProviderError, ProviderName, Request, Response,
        Result as LlmResult, StreamEvent,
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
        Agent::new(Box::new(MockProvider), Vec::new())
    }

    fn run(args: &str) -> Result<CommandOutcome, AppError> {
        Plan.execute(&mut fresh_agent(), args)
    }

    #[test]
    fn no_args_reports_current_mode_and_list() {
        let mut agent = fresh_agent();
        agent.set_todos(oven_agent::TodoList {
            items: vec![oven_agent::TodoItem {
                id: "a".into(),
                content: "one".into(),
                status: oven_agent::TodoStatus::Pending,
            }],
        });
        let out = Plan.execute(&mut agent, "").unwrap();
        let CommandOutcome::Reply(text) = out else {
            panic!("expected Reply, got {out:?}");
        };
        assert!(text.contains("current mode: agent"));
        assert!(text.contains("1 todos (0 in_progress, 0 completed)"));
    }

    #[test]
    fn on_requests_plan_mode() {
        let out = run("on").unwrap();
        assert!(matches!(
            out,
            CommandOutcome::ModeChanged {
                mode: AgentMode::Plan
            }
        ));
    }

    #[test]
    fn off_requests_default_mode() {
        let out = run("OFF").unwrap();
        assert!(matches!(
            out,
            CommandOutcome::ModeChanged {
                mode: AgentMode::Default
            }
        ));
    }

    #[test]
    fn invalid_args_error() {
        let err = run("maybe").unwrap_err();
        assert!(err.to_string().contains("usage: /plan"));
    }
}

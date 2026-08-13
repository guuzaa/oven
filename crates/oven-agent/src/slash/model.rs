use super::{CommandOutcome, SlashCommand};
use crate::agent::Agent;
use crate::error::AgentError;
use oven_llm::ReasoningEffort;

pub struct Model;

impl Model {
    fn parse_effort(s: &str) -> Result<ReasoningEffort, AgentError> {
        match s.to_ascii_lowercase().as_str() {
            "none" => Ok(ReasoningEffort::None),
            "low" => Ok(ReasoningEffort::Low),
            "medium" => Ok(ReasoningEffort::Medium),
            "high" => Ok(ReasoningEffort::High),
            _ => Err(AgentError::from(format!(
                "invalid reasoning effort '{s}'; expected none, low, medium, or high"
            ))),
        }
    }
}

impl SlashCommand for Model {
    fn name(&self) -> &str {
        "model"
    }

    fn description(&self) -> &str {
        "Switch model and reasoning effort: /model <id> [none|low|medium|high]"
    }

    fn execute(&self, agent: &mut Agent, args: &str) -> Result<CommandOutcome, AgentError> {
        let tokens: Vec<&str> = args.split_whitespace().collect();
        match tokens.as_slice() {
            [] => {
                let effort = match agent.reasoning_effort() {
                    Some(e) => crate::agent::effort_label(e).to_string(),
                    None => "default".to_string(),
                };
                Ok(CommandOutcome::Reply(format!(
                    "current model: {} (reasoning effort: {effort})",
                    agent.model().as_str()
                )))
            }
            [model] => Ok(CommandOutcome::ModelChanged {
                model: (*model).to_string(),
                reasoning_effort: agent.reasoning_effort(),
            }),
            [model, effort] => Ok(CommandOutcome::ModelChanged {
                model: (*model).to_string(),
                reasoning_effort: Some(Self::parse_effort(effort)?),
            }),
            _ => Err(AgentError::from(
                "usage: /model <id> [none|low|medium|high]".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
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

    fn run(args: &str) -> Result<CommandOutcome, AgentError> {
        Model.execute(&mut fresh_agent(), args)
    }

    #[test]
    fn no_args_reports_current_settings() {
        let out = run("").unwrap();
        let CommandOutcome::Reply(text) = out else {
            panic!("expected Reply, got {out:?}");
        };
        assert!(text.contains("current model: default"));
        assert!(text.contains("default"));
    }

    #[test]
    fn model_only_keeps_current_effort() {
        let mut agent = fresh_agent();
        agent.set_reasoning_effort(Some(ReasoningEffort::High));
        let out = Model.execute(&mut agent, "deepseek-chat").unwrap();
        assert!(matches!(
            out,
            CommandOutcome::ModelChanged {
                ref model,
                reasoning_effort: Some(ReasoningEffort::High),
            } if model == "deepseek-chat"
        ));
    }

    #[test]
    fn effort_is_parsed_case_insensitively() {
        for (raw, expected) in [
            ("none", ReasoningEffort::None),
            ("LOW", ReasoningEffort::Low),
            ("Medium", ReasoningEffort::Medium),
            ("high", ReasoningEffort::High),
        ] {
            let out = run(&format!("gpt-4o {raw}")).unwrap();
            assert!(
                matches!(
                    out,
                    CommandOutcome::ModelChanged {
                        reasoning_effort: Some(e),
                        ..
                    } if e == expected
                ),
                "{raw}"
            );
        }
    }

    #[test]
    fn invalid_effort_errors() {
        let err = run("gpt-4o turbo").unwrap_err();
        assert!(err.message.contains("invalid reasoning effort"));
    }

    #[test]
    fn too_many_tokens_error() {
        let err = run("gpt-4o high extra").unwrap_err();
        assert!(err.message.contains("usage:"));
    }
}

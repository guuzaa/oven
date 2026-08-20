use oven_agent::Agent;
use oven_llm::{ModelId, ReasoningEffort, RouterError};

use super::{CommandOutcome, SlashCommand};
use crate::AppError;

pub struct Model;

impl Model {
    fn parse_effort(s: &str) -> Result<ReasoningEffort, AppError> {
        match s.to_ascii_lowercase().as_str() {
            "none" => Ok(ReasoningEffort::None),
            "low" => Ok(ReasoningEffort::Low),
            "medium" => Ok(ReasoningEffort::Medium),
            "high" => Ok(ReasoningEffort::High),
            _ => Err(AppError::Runtime(format!(
                "invalid reasoning effort '{s}'; expected none, low, medium, or high"
            ))),
        }
    }

    pub(crate) fn qualify(agent: &Agent, raw: &str) -> Result<String, AppError> {
        let id = ModelId::from(raw);
        let qualified = agent.router().qualify(&id);
        if qualified.vendor().is_some() {
            match agent.router().provider(&qualified) {
                Ok(_) => Ok(qualified.to_string()),
                Err(RouterError::UnknownModel(_)) | Err(RouterError::NoProviderRegistered) => {
                    Err(AppError::Runtime(format!(
                        "model '{qualified}' is not available; run /setup to configure that provider"
                    )))
                }
                Err(e) => Err(AppError::Provider(e.to_string())),
            }
        } else {
            Ok(qualified.to_string())
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

    fn execute(&self, agent: &mut Agent, args: &str) -> Result<CommandOutcome, AppError> {
        let tokens: Vec<&str> = args.split_whitespace().collect();
        match tokens.as_slice() {
            [] => {
                let effort = match agent.reasoning_effort() {
                    Some(e) => e.to_string(),
                    None => "none".to_string(),
                };
                Ok(CommandOutcome::Reply(format!(
                    "current model: {} (reasoning effort: {effort})",
                    agent.model().as_str()
                )))
            }
            [model] => Ok(CommandOutcome::ModelChanged {
                model: Self::qualify(agent, model)?,
                reasoning_effort: agent.reasoning_effort(),
            }),
            [model, effort] => Ok(CommandOutcome::ModelChanged {
                model: Self::qualify(agent, model)?,
                reasoning_effort: Some(Self::parse_effort(effort)?),
            }),
            _ => Err(AppError::Runtime(
                "usage: /model <id> [none|low|medium|high]".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use oven_llm::{
        ModelInfo, Provider, ProviderError, ProviderName, Request, Response, Result as LlmResult,
        Router, StreamEvent,
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
            ProviderName::DeepSeek
        }
    }

    fn fresh_agent() -> Agent {
        let mut router = Router::new();
        router.register(Box::new(MockProvider));
        Agent::new(router, Vec::new())
    }

    fn run(args: &str) -> Result<CommandOutcome, AppError> {
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
            } if model == "deepseek/deepseek-chat"
        ));
    }

    #[test]
    fn qualifies_bare_id_and_vendor_alias() {
        let out = run("deepseek-v4-flash").unwrap();
        assert!(matches!(
            out,
            CommandOutcome::ModelChanged { ref model, .. } if model == "deepseek/deepseek-v4-flash"
        ));
        let out = run("deepseek/deepseek-v4-flash:responses").unwrap();
        assert!(matches!(
            out,
            CommandOutcome::ModelChanged { ref model, .. } if model == "deepseek/deepseek-v4-flash:responses"
        ));
    }

    #[test]
    fn cross_vendor_slug_errors() {
        let err = run("xai/grok-4.6").unwrap_err();
        assert!(err.to_string().contains("run /setup"));
        let err = run("grok/grok-4.6").unwrap_err();
        assert!(err.to_string().contains("xai/grok-4.6"));
        assert!(err.to_string().contains("run /setup"));
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
                        ref model,
                        reasoning_effort: Some(e),
                    } if e == expected && model == "deepseek/gpt-4o"
                ),
                "{raw}"
            );
        }
    }

    #[test]
    fn invalid_effort_errors() {
        let err = run("gpt-4o turbo").unwrap_err();
        assert!(err.to_string().contains("invalid reasoning effort"));
    }

    #[test]
    fn too_many_tokens_error() {
        let err = run("gpt-4o high extra").unwrap_err();
        assert!(err.to_string().contains("usage:"));
    }
}

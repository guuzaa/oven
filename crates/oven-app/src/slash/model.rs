use oven_agent::Agent;
use oven_llm::{ModelId, ReasoningEffort, Router, RouterError};

use super::{CommandOutcome, SlashCommand};
use crate::AppError;

const USAGE: &str = "usage: /model <id> [none|low|medium|high]";

pub struct Model;

/// The result of parsing `/model` arguments: either a request to report
/// the current model, or a validated switch.
pub(crate) enum ModelDirective {
    Query,
    Switch {
        model: String,
        reasoning_effort: Option<ReasoningEffort>,
    },
}

impl Model {
    pub(crate) const NAME: &'static str = "model";

    pub(crate) fn parse_effort(s: &str) -> Result<ReasoningEffort, AppError> {
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

    pub(crate) fn qualify_against(router: &Router, raw: &str) -> Result<String, AppError> {
        let id = ModelId::from(raw);
        let qualified = router.qualify(&id);
        if qualified.vendor().is_some() {
            match router.provider(&qualified) {
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

    /// Renders the "current model" summary shared by the idle `/model`
    /// reply and the mid-turn query path (see `runtime::apply_model_during_turn`).
    pub(crate) fn describe(model: &str, reasoning_effort: Option<ReasoningEffort>) -> String {
        let effort = reasoning_effort.map_or_else(|| "none".to_string(), |e| e.to_string());
        format!("current model: {model} (reasoning effort: {effort})")
    }

    /// Parses `/model` arguments against `router`, qualifying any model id
    /// without requiring `&mut Agent`. `current_effort` is kept when only a
    /// model id is given.
    pub(crate) fn resolve(
        router: &Router,
        current_effort: Option<ReasoningEffort>,
        args: &str,
    ) -> Result<ModelDirective, AppError> {
        let tokens: Vec<&str> = args.split_whitespace().collect();
        let (raw_model, reasoning_effort) = match tokens.as_slice() {
            [] => return Ok(ModelDirective::Query),
            [model] => (*model, current_effort),
            [model, effort] => (*model, Some(Self::parse_effort(effort)?)),
            _ => return Err(AppError::Runtime(USAGE.to_string())),
        };
        Ok(ModelDirective::Switch {
            model: Self::qualify_against(router, raw_model)?,
            reasoning_effort,
        })
    }
}

impl SlashCommand for Model {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn description(&self) -> &str {
        "Switch model and reasoning effort: /model <id> [none|low|medium|high]"
    }

    fn execute(&self, agent: &mut Agent, args: &str) -> Result<CommandOutcome, AppError> {
        match Self::resolve(&agent.router(), agent.reasoning_effort(), args)? {
            ModelDirective::Query => Ok(CommandOutcome::Reply(Self::describe(
                agent.model().as_str(),
                agent.reasoning_effort(),
            ))),
            ModelDirective::Switch {
                model,
                reasoning_effort,
            } => Ok(CommandOutcome::ModelChanged {
                model,
                reasoning_effort,
            }),
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

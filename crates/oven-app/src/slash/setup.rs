use oven_agent::Agent;

use super::{CommandOutcome, SlashCommand};
use crate::AppError;
use crate::config::ProviderConfig;

pub struct Setup;

impl Setup {
    fn parse(args: &str) -> Result<ProviderConfig, AppError> {
        let mut cfg = ProviderConfig::default();
        for token in args.split_whitespace() {
            let Some((key, value)) = token.split_once('=') else {
                return Err(AppError::Runtime(format!(
                    "invalid argument '{token}'; usage: /setup name=... kind=... api_key=..."
                )));
            };
            if value.is_empty() {
                return Err(AppError::Runtime(format!("empty value for '{key}'")));
            }
            match key {
                "name" => cfg.name = Some(value.to_string()),
                "api_key" => cfg.api_key = Some(value.to_string()),
                "kind" => {
                    cfg.kind = Some(ProviderConfig::parse_kind(value).ok_or_else(|| {
                        AppError::Runtime(format!(
                            "invalid kind '{value}'; expected completions or responses"
                        ))
                    })?);
                }
                _ => {
                    return Err(AppError::Runtime(format!(
                        "unknown field '{key}'; expected name, kind, api_key"
                    )));
                }
            }
        }
        Ok(cfg)
    }
}

impl SlashCommand for Setup {
    fn name(&self) -> &str {
        "setup"
    }

    fn description(&self) -> &str {
        "Configure provider: /setup name=... kind=... api_key=..."
    }

    fn execute(&self, agent: &mut Agent, args: &str) -> Result<CommandOutcome, AppError> {
        if args.trim().is_empty() {
            return Ok(CommandOutcome::Reply(format!(
                "current model: {}\nusage: /setup name=<provider> kind=<completions|responses> api_key=<key>",
                agent.model().as_str()
            )));
        }
        Ok(CommandOutcome::ProviderChanged {
            provider: Self::parse(args)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use oven_llm::{
        ModelId, ModelInfo, Provider, ProviderError, ProviderKind, ProviderName, Request, Response,
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
        Setup.execute(&mut fresh_agent(), args)
    }

    #[test]
    fn no_args_reports_usage() {
        let out = run("").unwrap();
        let CommandOutcome::Reply(text) = out else {
            panic!("expected Reply, got {out:?}");
        };
        assert!(text.contains("current model: default"));
        assert!(text.contains("usage: /setup"));
    }

    #[test]
    fn parses_all_fields() {
        let out = run("name=deepseek kind=responses api_key=sk-test").unwrap();
        match out {
            CommandOutcome::ProviderChanged { provider } => {
                assert_eq!(provider.name.as_deref(), Some("deepseek"));
                assert_eq!(provider.kind, Some(ProviderKind::Responses));
                assert_eq!(provider.api_key.as_deref(), Some("sk-test"));
                assert!(provider.model.is_none());
                assert!(provider.base_url.is_none());
            }
            other => panic!("expected ProviderChanged, got {other:?}"),
        }
    }

    #[test]
    fn partial_update_leaves_unset_fields() {
        let out = run("kind=completions").unwrap();
        match out {
            CommandOutcome::ProviderChanged { provider } => {
                assert_eq!(provider.kind, Some(ProviderKind::Completions));
                assert!(provider.name.is_none());
                assert!(provider.model.is_none());
                assert!(provider.base_url.is_none());
                assert!(provider.api_key.is_none());
            }
            other => panic!("expected ProviderChanged, got {other:?}"),
        }
    }

    #[test]
    fn invalid_kind_errors() {
        let err = run("kind=chat").unwrap_err();
        assert!(err.to_string().contains("invalid kind"));
    }

    #[test]
    fn unknown_field_errors() {
        let err = run("foo=bar").unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn bare_token_errors() {
        let err = run("deepseek").unwrap_err();
        assert!(err.to_string().contains("invalid argument"));
    }

    #[test]
    fn empty_value_errors() {
        let err = run("name=").unwrap_err();
        assert!(err.to_string().contains("empty value"));
    }
}

use oven_agent::Agent;
use oven_llm::canonical_vendor;

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
                    "invalid argument '{token}'; usage: /setup name=... api_key=..."
                )));
            };
            if value.is_empty() {
                return Err(AppError::Runtime(format!("empty value for '{key}'")));
            }
            match key {
                "name" => cfg.name = Some(canonical_vendor(value)),
                "api_key" => cfg.api_key = Some(value.to_string()),
                "base_url" => cfg.base_url = Some(value.to_string()),
                "protocol" => {
                    cfg.protocol =
                        Some(ProviderConfig::parse_protocol(value).ok_or_else(|| {
                            AppError::Runtime(format!(
                                "invalid protocol '{value}'; expected completions or responses"
                            ))
                        })?);
                }
                "kind" => {
                    return Err(AppError::Runtime(
                        "kind is no longer used; known providers pick a protocol automatically"
                            .into(),
                    ));
                }
                _ => {
                    return Err(AppError::Runtime(format!(
                        "unknown field '{key}'; expected name, api_key, base_url, protocol"
                    )));
                }
            }
        }
        cfg.normalize();
        Ok(cfg)
    }
}

impl SlashCommand for Setup {
    fn name(&self) -> &str {
        "setup"
    }

    fn description(&self) -> &str {
        "Configure provider: /setup name=... api_key=..."
    }

    fn execute(&self, agent: &mut Agent, args: &str) -> Result<CommandOutcome, AppError> {
        if args.trim().is_empty() {
            return Ok(CommandOutcome::Reply(format!(
                "current model: {}\nusage: /setup name=<provider> api_key=<key>",
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
        assert!(!text.contains("kind="));
    }

    #[test]
    fn parses_all_fields() {
        let out = run("name=deepseek api_key=sk-test").unwrap();
        match out {
            CommandOutcome::ProviderChanged { provider } => {
                assert_eq!(provider.name.as_deref(), Some("deepseek"));
                assert!(provider.protocol.is_none());
                assert_eq!(provider.api_key.as_deref(), Some("sk-test"));
                assert!(provider.model.is_none());
                assert!(provider.base_url.is_none());
            }
            other => panic!("expected ProviderChanged, got {other:?}"),
        }
    }

    #[test]
    fn grok_name_canonicalizes_to_xai() {
        let out = run("name=grok api_key=xai-key").unwrap();
        match out {
            CommandOutcome::ProviderChanged { provider } => {
                assert_eq!(provider.name.as_deref(), Some("xai"));
            }
            other => panic!("expected ProviderChanged, got {other:?}"),
        }
    }

    #[test]
    fn custom_protocol_is_kept() {
        let out = run("name=my-proxy protocol=responses api_key=sk").unwrap();
        match out {
            CommandOutcome::ProviderChanged { provider } => {
                assert_eq!(provider.name.as_deref(), Some("my-proxy"));
                assert_eq!(provider.protocol, Some(ProviderKind::Responses));
            }
            other => panic!("expected ProviderChanged, got {other:?}"),
        }
    }

    #[test]
    fn kind_is_rejected() {
        let err = run("kind=chat").unwrap_err();
        assert!(err.to_string().contains("kind is no longer used"));
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

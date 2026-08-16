use async_trait::async_trait;
use futures::stream::BoxStream;
use oven_agent::RetryingProvider;
use oven_llm::{
    ModelId, ModelInfo, Provider, ProviderBuilder, ProviderError, ProviderName, Request, Response,
    StreamEvent,
};

use crate::AppError;
use crate::config::AppConfig;

pub(crate) fn build_provider(
    config: &AppConfig,
    model: &str,
) -> Result<Box<dyn Provider>, AppError> {
    let provider = &config.provider;
    let provider_name = provider.effective_provider_name(model);
    let api_key = provider.effective_api_key();
    let base_url = provider.effective_base_url();

    if base_url.is_none() {
        match &provider_name {
            ProviderName::Anthropic => {
                return Err(AppError::Provider(format!(
                    "model '{model}' needs an OpenAI-compatible proxy; set OVEN_BASE_URL or provider.base_url"
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

    let builder = ProviderBuilder::new(provider.effective_kind())
        .provider_name(provider_name)
        .api_key(api_key);
    let provider = match &base_url {
        Some(u) => builder.base_url(u),
        None => builder,
    };

    let retrying = RetryingProvider::new(provider.build()?)
        .with_timeout(config.request_timeout())
        .with_retries(config.max_retries)
        .with_base_backoff(config.base_backoff());
    Ok(Box::new(retrying))
}

pub(crate) fn build_interactive_provider(
    config: &AppConfig,
    model: &str,
) -> Result<Box<dyn Provider>, AppError> {
    if config.provider.needs_setup() {
        return Ok(Box::new(UnconfiguredProvider));
    }
    build_provider(config, model)
}

struct UnconfiguredProvider;

#[async_trait]
impl Provider for UnconfiguredProvider {
    async fn complete(&self, _req: &Request) -> Result<Response, ProviderError> {
        Err(ProviderError::Auth(
            "run /setup to configure a provider".into(),
        ))
    }

    async fn stream(
        &self,
        _req: &Request,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        Err(ProviderError::Auth(
            "run /setup to configure a provider".into(),
        ))
    }

    fn resolve_model(&self, _id: &ModelId) -> Option<&ModelInfo> {
        None
    }

    fn provider_name(&self) -> ProviderName {
        ProviderName::Custom("unconfigured".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_provider_is_placeholder_without_key() {
        let cfg = AppConfig::default();
        if !cfg.provider.needs_setup() {
            return;
        }
        let provider = build_interactive_provider(&cfg, "deepseek-v4-flash").unwrap();
        assert_eq!(
            provider.provider_name(),
            ProviderName::Custom("unconfigured".into())
        );
    }
}

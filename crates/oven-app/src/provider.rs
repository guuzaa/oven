use oven_agent::RetryingProvider;
use oven_llm::{Provider, ProviderBuilder, ProviderKind, ProviderName, Router};

use crate::AppError;
use crate::config::{AppConfig, ProviderConfig};

pub(crate) fn retrying(config: &AppConfig, client: Box<dyn Provider>) -> Box<dyn Provider> {
    Box::new(
        RetryingProvider::new(client)
            .with_timeout(config.request_timeout())
            .with_retries(config.max_retries)
            .with_base_backoff(config.base_backoff()),
    )
}

pub(crate) fn build_router(config: &AppConfig) -> Result<Router, AppError> {
    let mut router = Router::new();
    let mut last_err = None;
    let mut registered = 0usize;
    for provider in config.registerable_providers() {
        match build_client(provider) {
            Ok(client) => {
                router.register(retrying(config, client));
                registered += 1;
            }
            Err(e) => last_err = Some(e),
        }
    }
    if registered == 0 {
        return Err(last_err.unwrap_or_else(|| {
            AppError::Provider(
                "no API key for any provider; set provider.api_key or run /setup".into(),
            )
        }));
    }
    Ok(router)
}

pub(crate) fn build_interactive_router(config: &AppConfig) -> Result<Router, AppError> {
    if config.needs_setup() {
        return Ok(Router::new());
    }
    build_router(config)
}

pub(crate) fn build_client(provider: &ProviderConfig) -> Result<Box<dyn Provider>, AppError> {
    let provider_name = provider.effective_provider_name();
    let api_key = provider.effective_api_key();
    let base_url = provider.effective_base_url();
    let model = provider.effective_model();

    match &provider_name {
        ProviderName::Anthropic if base_url.is_none() => {
            return Err(AppError::Provider(format!(
                "model '{model}' needs an OpenAI-compatible proxy; set OVEN_BASE_URL or provider.base_url"
            )));
        }
        ProviderName::Custom(_) if base_url.is_none() => {
            return Err(AppError::Provider(format!(
                "unknown provider for model '{model}'; set provider.base_url or OVEN_BASE_URL to use an OpenAI-compatible endpoint"
            )));
        }
        _ => {}
    }
    if base_url.is_none() && api_key.is_empty() {
        return Err(AppError::Provider(format!(
            "no API key for model '{model}'; set the matching API key env var or provider.api_key"
        )));
    }

    let custom_protocol = match &provider_name {
        ProviderName::Custom(_) | ProviderName::Anthropic => {
            provider.protocol.or(Some(ProviderKind::Completions))
        }
        _ => None,
    };
    let mut builder = match custom_protocol {
        Some(kind) => ProviderBuilder::new(kind),
        None => ProviderBuilder::provider(),
    };
    builder = builder.provider_name(provider_name).api_key(api_key);
    if let Some(u) = &base_url {
        builder = builder.base_url(u);
    }
    Ok(builder.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oven_llm::{ModelId, RouterError};

    #[test]
    fn interactive_router_is_empty_without_key() {
        let cfg = AppConfig::default();
        if !cfg.needs_setup() {
            return;
        }
        let router = build_interactive_router(&cfg).unwrap();
        assert!(matches!(
            router.provider(&ModelId::from("anything")),
            Err(RouterError::NoProviderRegistered)
        ));
    }
}

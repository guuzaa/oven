use std::time::{Duration, Instant};

use oven_agent::{ModelCatalog, RouterHandle};
use oven_llm::{
    ModelInfo, Provider, ProviderBuilder, ProviderName, Request, RetryingProvider, Router,
};

use crate::core::config::{AppConfig, ModelMetadata, ProviderConfig};
use crate::core::error::AppError;

/// The one-token request [`verify`] sends.
const VERIFY_PROMPT: &str = "ping";

/// `ModelInfo` for a user-declared model. Unset capabilities stay supported
/// and unknown limits stay zeroed: validation must not reject a model for
/// metadata the user chose not (or was unable) to spell out.
fn declared_model_info(
    id: &str,
    params: &ModelMetadata,
    provider_name: &ProviderName,
) -> ModelInfo {
    let mut info = ModelInfo::declared(id, provider_name.clone());
    if let Some(tokens) = params.context_window {
        info.context_window = tokens;
    }
    if let Some(tokens) = params.max_output_tokens {
        info.max_output_tokens = tokens;
    }
    info.capabilities = info.capabilities.with_overrides(
        params.supports_vision,
        params.supports_tools,
        params.supports_streaming,
        params.supports_system_prompt,
    );
    info
}

pub(crate) fn build_router(config: &AppConfig) -> Result<Router, AppError> {
    let mut router = Router::new();
    let mut last_err = None;
    let mut registered = 0usize;
    for provider in config.registerable_providers() {
        match build_client(provider) {
            Ok(client) => {
                router.register(
                    RetryingProvider::new(client)
                        .with_timeout(config.request_timeout())
                        .with_retries(config.max_retries)
                        .with_base_backoff(config.base_backoff()),
                );
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
    build_client_with(provider, &provider.effective_api_key())
}

/// Models `oven-llm` ships for this provider's vendor. The static catalog
/// needs neither a credential nor a client.
pub fn provider_catalog(provider: &ProviderConfig) -> Vec<ModelInfo> {
    provider.effective_provider_name().known_models()
}

/// The models the app's shared router can serve, read live so a `/setup`
/// swap is picked up. Subagent model validation reads the same router.
pub(crate) struct RouterCatalog(RouterHandle);

impl RouterCatalog {
    pub(crate) fn new(router: RouterHandle) -> Self {
        Self(router)
    }
}

impl ModelCatalog for RouterCatalog {
    fn models(&self) -> Vec<ModelInfo> {
        self.0.load().known_models()
    }
}

/// Models the endpoint reports on `GET /models`. Unlike the static catalog
/// this needs a real credential, so a failure is the caller's to report.
pub async fn provider_models(
    provider: &ProviderConfig,
    timeout: Duration,
) -> Result<Vec<ModelInfo>, AppError> {
    let client = build_client(provider)?;
    tokio::time::timeout(timeout, client.list_models())
        .await
        .map_err(|_| AppError::Provider(format!("no model list within {}s", timeout.as_secs())))?
        .map_err(AppError::from)
}

/// Prove an endpoint, key and model id work together: one token through a
/// freshly built client, so `model add` can fail before it saves a config.
pub async fn verify(provider: &ProviderConfig, timeout: Duration) -> Result<Duration, AppError> {
    let client = build_client(provider)?;
    let request = Request::builder()
        .model(provider.effective_model())
        .prompt(VERIFY_PROMPT)
        .max_tokens(1)
        .build()
        .map_err(|e| AppError::Provider(e.to_string()))?;
    let started = Instant::now();
    tokio::time::timeout(timeout, client.complete(&request))
        .await
        .map_err(|_| AppError::Provider(format!("no response within {}s", timeout.as_secs())))??;
    Ok(started.elapsed())
}

fn build_client_with(
    provider: &ProviderConfig,
    api_key: &str,
) -> Result<Box<dyn Provider>, AppError> {
    let provider_name = provider.effective_provider_name();
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

    let mut builder = match provider.protocol {
        Some(kind) => ProviderBuilder::new(kind),
        None => ProviderBuilder::provider(),
    };
    for (id, params) in provider.effective_models() {
        builder = builder.add_model(declared_model_info(id, &params, &provider_name));
    }
    builder = builder.provider_name(provider_name).api_key(api_key);
    if let Some(u) = &base_url {
        builder = builder.base_url(u);
    }
    Ok(builder.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use oven_llm::{ModelId, RouterError};

    fn declared<const N: usize>(
        entries: [(&str, ModelMetadata); N],
    ) -> BTreeMap<String, ModelMetadata> {
        entries
            .into_iter()
            .map(|(id, metadata)| (id.to_owned(), metadata))
            .collect()
    }

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

    #[test]
    fn configured_model_params_resolve_via_provider() {
        let provider = ProviderConfig {
            name: Some("myproxy".into()),
            base_url: Some("https://example.com/v1".into()),
            api_key: Some("k".into()),
            model: Some("my-model".into()),
            models: declared([(
                "my-model",
                ModelMetadata {
                    context_window: Some(200_000),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let client = build_client(&provider).unwrap();
        let info = client
            .resolve_model(&ModelId::from("my-model"))
            .expect("configured model should resolve");
        assert_eq!(info.context_window, 200_000);
    }

    #[test]
    fn configured_model_params_override_preset_catalog() {
        let provider = ProviderConfig {
            name: Some("deepseek".into()),
            api_key: Some("k".into()),
            models: declared([(
                "deepseek-v4-flash",
                ModelMetadata {
                    context_window: Some(42_000),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let client = build_client(&provider).unwrap();
        let info = client
            .resolve_model(&ModelId::from("deepseek-v4-flash"))
            .expect("preset model should resolve");
        assert_eq!(info.context_window, 42_000);
    }

    #[test]
    fn declared_model_defaults_to_supported_capabilities() {
        let provider = ProviderConfig {
            name: Some("stepfun".into()),
            base_url: Some("https://api.stepfun.com/v1".into()),
            api_key: Some("k".into()),
            model: Some("step-5-preview".into()),
            models: declared([(
                "step-5-preview",
                ModelMetadata {
                    context_window: Some(1_000_000),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let client = build_client(&provider).unwrap();
        let info = client
            .resolve_model(&ModelId::from("step-5-preview"))
            .expect("declared model should resolve");
        assert!(info.capabilities.supports_system_prompt);
        assert!(info.capabilities.supports_tools);
        assert!(info.capabilities.supports_streaming);
        assert!(info.capabilities.supports_vision);
        assert_eq!(info.max_output_tokens, 0);
    }

    #[test]
    fn declared_model_limits_and_capabilities_are_configurable() {
        let provider = ProviderConfig {
            name: Some("myproxy".into()),
            base_url: Some("https://example.com/v1".into()),
            api_key: Some("k".into()),
            model: Some("my-model".into()),
            models: declared([(
                "my-model",
                ModelMetadata {
                    max_output_tokens: Some(8192),
                    supports_vision: Some(false),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let client = build_client(&provider).unwrap();
        let info = client
            .resolve_model(&ModelId::from("my-model"))
            .expect("declared model should resolve");
        assert_eq!(info.max_output_tokens, 8192);
        assert!(!info.capabilities.supports_vision);
        assert!(info.capabilities.supports_tools);
    }

    #[test]
    fn provider_metadata_reaches_the_built_client() {
        let config: AppConfig = toml::from_str(
            r#"
active = "myproxy"

[providers.myproxy]
base_url = "https://example.com/v1"
api_key = "k"
model = "shared"
context_window = 200000
supports_vision = false

[providers.myproxy.models.shared]
max_output_tokens = 4096
"#,
        )
        .unwrap();
        let provider = config.active_provider_config().unwrap();
        let client = build_client(provider).unwrap();
        let info = client
            .resolve_model(&ModelId::from("shared"))
            .expect("declared model should resolve");
        assert_eq!(info.context_window, 200_000);
        assert_eq!(info.max_output_tokens, 4096);
        assert!(!info.capabilities.supports_vision);
    }

    #[test]
    fn provider_catalog_reads_the_static_table_without_a_key() {
        let provider = ProviderConfig {
            name: Some("deepseek".into()),
            ..Default::default()
        };
        let ids: Vec<_> = provider_catalog(&provider)
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert!(ids.iter().any(|id| id == "deepseek-v4-flash"), "{ids:?}");
    }

    #[test]
    fn provider_catalog_is_empty_for_a_custom_vendor() {
        let provider = ProviderConfig {
            name: Some("myproxy".into()),
            base_url: Some("https://example.com/v1".into()),
            ..Default::default()
        };
        assert!(provider_catalog(&provider).is_empty());
    }

    #[tokio::test]
    async fn verify_reports_an_unreachable_endpoint() {
        let provider = ProviderConfig {
            name: Some("offline".into()),
            base_url: Some("http://127.0.0.1:9/v1".into()),
            api_key: Some("k".into()),
            model: Some("m".into()),
            ..Default::default()
        };
        let error = verify(&provider, Duration::from_secs(5))
            .await
            .expect_err("a closed port must not pass verification");
        assert!(matches!(error, AppError::Provider(_)), "{error:?}");
    }
}

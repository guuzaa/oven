//! `oven model add`: an onboarding walk that turns answers into one provider
//! table. Every question is also a flag, so the same code serves a script.

use std::fmt::Write as _;
use std::time::Duration;

use clap::Args as ClapArgs;
use oven_app::config::{AppConfig, ModelMetadata, ProviderConfig};
use oven_app::{AppError, provider_catalog, provider_models, verify};
use oven_llm::{ModelInfo, ProviderKind, ReasoningEffort, canonical_vendor};

use super::Context;
use crate::commands::prompt::{Prompter, Stdin, choose, required_text};

/// How long the endpoint's model list and the verification request may take.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(5);
const VERIFY_TIMEOUT: Duration = Duration::from_secs(15);

/// Preset vendors the walk offers, in menu order.
const VENDORS: [&str; 5] = ["deepseek", "openai", "moonshot", "zhipu", "xai"];
const CUSTOM: &str = "custom";
const DEFAULT_PROTOCOL: ProviderKind = ProviderKind::Completions;

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// Provider slug to configure; asked when omitted
    provider: Option<String>,
    /// Endpoint URL; required for a vendor oven does not ship
    #[arg(long)]
    base_url: Option<String>,
    /// `completions` or `responses`; only an unknown vendor keeps it
    #[arg(long)]
    protocol: Option<String>,
    /// API key to save
    #[arg(long)]
    api_key: Option<String>,
    /// Read the key from `OVEN_API_KEY` instead of saving it
    #[arg(long)]
    api_key_env: bool,
    /// Wire id of the model to add
    #[arg(long)]
    model: Option<String>,
    /// Context window in tokens; `200k` and `1m` are accepted
    #[arg(long)]
    context_window: Option<String>,
    /// Max output tokens; `200k` and `1m` are accepted
    #[arg(long)]
    max_output_tokens: Option<String>,
    #[arg(long)]
    no_tools: bool,
    #[arg(long)]
    no_vision: bool,
    /// Reasoning effort: none, low, medium or high
    #[arg(long)]
    reasoning_effort: Option<String>,
    /// Make the added provider active and use its model
    #[arg(long)]
    activate: bool,
    /// Skip the one-token connectivity check
    #[arg(long)]
    no_verify: bool,
    /// Write without showing the preview
    #[arg(long, short = 'y')]
    yes: bool,
}

/// A provider table ready to write, and whether writing it also selects it.
#[derive(Debug)]
struct Plan {
    slug: String,
    id: String,
    provider: ProviderConfig,
    activate: bool,
}

pub(crate) async fn run(ctx: &Context, args: &Args) -> Result<String, AppError> {
    let mut prompter = Stdin::new();
    let plan = plan(&mut prompter, ctx, args).await?;
    if !args.yes {
        prompter.say(&AppConfig::provider_toml(&plan.provider)?);
        let question = format!("write it to {}?", ctx.user_path()?.display());
        if !prompter.confirm(&question, true) {
            return Ok("aborted; nothing written".into());
        }
    }
    apply(ctx, &plan).await
}

async fn plan(p: &mut dyn Prompter, ctx: &Context, args: &Args) -> Result<Plan, AppError> {
    let slug = resolve_slug(p, ctx, args)?;
    let mut provider = ctx.config.providers.get(&slug).cloned().unwrap_or_default();
    provider.name = Some(slug.clone());
    let preset = !provider.is_custom_vendor();

    provider.base_url = resolve_base_url(p, &provider, args, preset)?;
    provider.protocol = resolve_protocol(p, &provider, args, preset)?;
    provider.api_key = resolve_key(p, &provider, args, preset)?;
    if let Some(raw) = &args.reasoning_effort {
        provider.reasoning_effort = Some(parse_effort(raw)?);
    }

    let remote = match args.model {
        Some(_) => Vec::new(),
        None => remote_ids(p, ctx, &provider).await,
    };
    let id = resolve_model(p, &provider, args, &remote)?;
    let shipped = provider_catalog(&provider)
        .into_iter()
        .find(|info| info.id == id);
    if let Some(metadata) = resolve_metadata(p, &provider, &id, args, shipped.as_ref())? {
        provider.models.insert(id.clone(), metadata);
    }

    // Selecting the provider is what makes the new model current, so it only
    // happens when asked or when nothing usable is selected already.
    let selected = ctx
        .config
        .active_provider_config()
        .is_some_and(|active| !active.needs_setup());
    let activate = args.activate || !selected;
    if activate || ctx.config.active_provider.name != slug {
        provider.model = Some(id.clone());
    }
    provider.normalize();

    if !args.no_verify {
        verify_draft(p, &provider, &slug, &id).await?;
    }
    Ok(Plan {
        slug,
        id,
        provider,
        activate,
    })
}

async fn verify_draft(
    p: &mut dyn Prompter,
    provider: &ProviderConfig,
    slug: &str,
    id: &str,
) -> Result<(), AppError> {
    let mut draft = provider.clone();
    draft.model = Some(id.to_string());
    match verify(&draft, VERIFY_TIMEOUT).await {
        Ok(elapsed) => {
            p.say(&format!(
                "verified {slug}/{id} in {} ms",
                elapsed.as_millis()
            ));
            Ok(())
        }
        Err(error) => {
            p.say(&format!("verification failed: {error}"));
            if p.confirm("save anyway?", false) {
                Ok(())
            } else {
                Err(AppError::Runtime("aborted; nothing written".into()))
            }
        }
    }
}

async fn apply(ctx: &Context, plan: &Plan) -> Result<String, AppError> {
    let path = ctx.user_path()?;
    let mut config = AppConfig::load_file(path)
        .await?
        .unwrap_or_else(AppConfig::empty);
    config
        .providers
        .insert(plan.slug.clone(), plan.provider.clone());
    if plan.activate {
        config.active_provider.name.clone_from(&plan.slug);
    }
    AppConfig::save_at(path, &config).await?;

    let mut text = format!("saved {}/{} to {}", plan.slug, plan.id, path.display());
    if plan.activate {
        let _ = write!(text, "\nactive model is now {}", active_model(&config));
    } else {
        let _ = write!(text, "\nactive model unchanged: {}", active_model(&config));
    }
    if ctx.project_declares(&plan.slug).await {
        let _ = write!(
            text,
            "\nnote: {} also declares {} and takes precedence",
            ctx.project_path.display(),
            plan.slug
        );
    }
    Ok(text)
}

fn active_model(config: &AppConfig) -> String {
    config
        .active_provider_config()
        .map(ProviderConfig::effective_model)
        .unwrap_or_else(|| "(none)".to_string())
}

fn resolve_slug(p: &mut dyn Prompter, ctx: &Context, args: &Args) -> Result<String, AppError> {
    if let Some(raw) = &args.provider {
        return check_slug(raw);
    }
    let mut items: Vec<String> = VENDORS.iter().map(|slug| (*slug).to_string()).collect();
    for slug in ctx.config.providers.keys() {
        if !items.contains(slug) {
            items.push(slug.clone());
        }
    }
    items.push(CUSTOM.to_string());
    let answer = choose(p, "provider", &items, Some(VENDORS[0]), "--provider")?;
    if answer == CUSTOM {
        return check_slug(&required_text(p, "provider name", None, "--provider")?);
    }
    check_slug(&answer)
}

fn check_slug(raw: &str) -> Result<String, AppError> {
    match canonical_vendor(raw.trim()) {
        slug if slug.is_empty() => Err(AppError::Runtime("provider name is required".into())),
        slug => Ok(slug),
    }
}

fn resolve_base_url(
    p: &mut dyn Prompter,
    provider: &ProviderConfig,
    args: &Args,
    preset: bool,
) -> Result<Option<String>, AppError> {
    if let Some(url) = &args.base_url {
        return Ok(Some(check_url(url)?));
    }
    if provider.base_url.is_some() {
        return Ok(provider.base_url.clone());
    }
    if preset {
        return Ok(None);
    }
    Ok(Some(check_url(&required_text(
        p,
        "base_url",
        None,
        "--base-url",
    )?)?))
}

fn check_url(raw: &str) -> Result<String, AppError> {
    let url = raw.trim();
    if url.starts_with("http://") || url.starts_with("https://") {
        return Ok(url.to_string());
    }
    Err(AppError::Runtime(format!(
        "invalid base_url '{raw}'; expected an http:// or https:// URL"
    )))
}

fn resolve_protocol(
    p: &mut dyn Prompter,
    provider: &ProviderConfig,
    args: &Args,
    preset: bool,
) -> Result<Option<ProviderKind>, AppError> {
    if preset {
        return Ok(None);
    }
    if let Some(raw) = &args.protocol {
        return Ok(Some(check_protocol(raw)?));
    }
    if let Some(kind) = provider.protocol {
        return Ok(Some(kind));
    }
    if !p.interactive() {
        return Ok(Some(DEFAULT_PROTOCOL));
    }
    let items = vec!["completions".to_string(), "responses".to_string()];
    let answer = choose(p, "protocol", &items, Some("completions"), "--protocol")?;
    Ok(Some(check_protocol(&answer)?))
}

fn check_protocol(raw: &str) -> Result<ProviderKind, AppError> {
    ProviderConfig::parse_protocol(raw).ok_or_else(|| {
        AppError::Runtime(format!(
            "invalid protocol '{raw}'; expected completions or responses"
        ))
    })
}

fn resolve_key(
    p: &mut dyn Prompter,
    provider: &ProviderConfig,
    args: &Args,
    preset: bool,
) -> Result<Option<String>, AppError> {
    if args.api_key_env {
        return Ok(None);
    }
    if let Some(key) = &args.api_key {
        return Ok(Some(check_key(key)));
    }
    let saved = provider.api_key.clone().filter(|key| !key.is_empty());
    let prompt = if saved.is_some() {
        "api_key (empty keeps the saved key)"
    } else {
        "api_key (empty uses OVEN_API_KEY)"
    };
    match p.secret(prompt) {
        Some(entered) if !entered.trim().is_empty() => Ok(Some(check_key(&entered))),
        Some(_) => Ok(saved),
        None if saved.is_some() => Ok(saved),
        None if preset && provider.effective_api_key().is_empty() => Err(AppError::Runtime(
            "--api-key is required for a preset vendor (or --api-key-env)".into(),
        )),
        None => Ok(None),
    }
}

/// A key pasted with its header, or with the whitespace a copy drags along,
/// must still end up usable.
fn check_key(raw: &str) -> String {
    let key = raw.trim();
    key.strip_prefix("Bearer ").unwrap_or(key).to_string()
}

/// What the endpoint says it serves. A failure is said out loud and the walk
/// carries on with the shipped catalog, which needs no network.
async fn remote_ids(p: &mut dyn Prompter, ctx: &Context, provider: &ProviderConfig) -> Vec<String> {
    if provider.effective_api_key().is_empty() {
        return Vec::new();
    }
    let timeout = ctx.config.request_timeout().min(REMOTE_TIMEOUT);
    match provider_models(provider, timeout).await {
        Ok(models) => models.into_iter().map(|model| model.id).collect(),
        Err(error) => {
            p.say(&format!("could not list models: {error}"));
            Vec::new()
        }
    }
}

fn resolve_model(
    p: &mut dyn Prompter,
    provider: &ProviderConfig,
    args: &Args,
    remote: &[String],
) -> Result<String, AppError> {
    if let Some(raw) = &args.model {
        return check_model(raw, provider);
    }
    let mut items = remote.to_vec();
    for info in provider_catalog(provider) {
        if !items.contains(&info.id) {
            items.push(info.id);
        }
    }
    let default = provider.model.clone().or_else(|| items.first().cloned());
    let answer = choose(p, "model id", &items, default.as_deref(), "--model")?;
    check_model(&answer, provider)
}

fn check_model(raw: &str, provider: &ProviderConfig) -> Result<String, AppError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(AppError::Runtime("model id is required".into()));
    }
    let slug = provider.name.as_deref().unwrap_or_default();
    match raw.split_once('/') {
        Some((vendor, wire)) if canonical_vendor(vendor) == slug && !wire.is_empty() => {
            Ok(wire.to_string())
        }
        Some((vendor, _)) => Err(AppError::Runtime(format!(
            "invalid model id '{raw}'; it names provider '{}', not '{slug}'",
            canonical_vendor(vendor)
        ))),
        None => Ok(raw.to_string()),
    }
}

/// What to declare under the model id, or `None` when a shipped model needs no
/// declaration at all. A declared entry replaces the catalog's rather than
/// merging with it, so an override of a shipped model is seeded with the whole
/// catalog entry: changing the window must not zero the output limit.
fn resolve_metadata(
    p: &mut dyn Prompter,
    provider: &ProviderConfig,
    id: &str,
    args: &Args,
    shipped: Option<&ModelInfo>,
) -> Result<Option<ModelMetadata>, AppError> {
    let mut metadata = provider.models.get(id).cloned().unwrap_or_default();
    if let Some(raw) = &args.context_window {
        metadata.context_window = Some(parse_tokens(raw)?);
    }
    if let Some(raw) = &args.max_output_tokens {
        metadata.max_output_tokens = Some(parse_tokens(raw)?);
    }
    if args.no_tools {
        metadata.supports_tools = Some(false);
    }
    if args.no_vision {
        metadata.supports_vision = Some(false);
    }

    // A model oven ships already has limits and capabilities; asking again
    // would only invite a wrong answer. A script answers nothing.
    if shipped.is_none() && p.interactive() {
        if metadata.context_window.is_none()
            && let Some(raw) = optional(p, "context_window (empty = unknown)")
        {
            metadata.context_window = Some(parse_tokens(&raw)?);
        }
        if metadata.max_output_tokens.is_none()
            && let Some(raw) = optional(p, "max_output_tokens (empty = unknown)")
        {
            metadata.max_output_tokens = Some(parse_tokens(&raw)?);
        }
        // Supported is the default, so only a "no" is worth writing down.
        if metadata.supports_tools.is_none() && !p.confirm("supports tools?", true) {
            metadata.supports_tools = Some(false);
        }
        if metadata.supports_vision.is_none() && !p.confirm("supports vision?", true) {
            metadata.supports_vision = Some(false);
        }
    }

    let Some(info) = shipped else {
        return Ok(Some(metadata));
    };
    let mut full = ModelMetadata::from_info(info);
    full.merge_fields(&metadata);
    Ok((full != ModelMetadata::from_info(info)).then_some(full))
}

fn optional(p: &mut dyn Prompter, prompt: &str) -> Option<String> {
    p.text(prompt, None).filter(|value| !value.is_empty())
}

fn parse_tokens(raw: &str) -> Result<u32, AppError> {
    let raw = raw.trim().to_ascii_lowercase();
    let (digits, scale) = match raw.strip_suffix('k') {
        Some(head) => (head, 1_000),
        None => match raw.strip_suffix('m') {
            Some(head) => (head, 1_000_000),
            None => (raw.as_str(), 1),
        },
    };
    digits
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(scale))
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            AppError::Runtime(format!(
                "invalid token count '{raw}'; expected a number, `200k` or `1m`"
            ))
        })
}

fn parse_effort(raw: &str) -> Result<ReasoningEffort, AppError> {
    ProviderConfig::parse_effort(raw).ok_or_else(|| {
        AppError::Runtime(format!(
            "invalid reasoning effort '{raw}'; expected none, low, medium or high"
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::commands::prompt::scripted::Scripted;

    fn context(config: &str) -> Context {
        Context {
            user_path: Some(PathBuf::from("/tmp/does-not-exist/config.toml")),
            project_path: PathBuf::from("/tmp/does-not-exist/.oven.toml"),
            config: toml::from_str(config).unwrap(),
        }
    }

    fn args(provider: &str) -> Args {
        Args {
            provider: Some(provider.to_string()),
            base_url: None,
            protocol: None,
            api_key: None,
            api_key_env: false,
            model: None,
            context_window: None,
            max_output_tokens: None,
            no_tools: false,
            no_vision: false,
            reasoning_effort: None,
            activate: false,
            no_verify: true,
            yes: true,
        }
    }

    #[tokio::test]
    async fn a_preset_vendor_only_needs_a_key_and_a_model() {
        let ctx = context("");
        let mut args = args("grok");
        args.model = Some("grok-4.6".into());
        let mut p = Scripted::new().secrets(["xai-key"]);
        let plan = plan(&mut p, &ctx, &args).await.unwrap();
        assert_eq!(plan.slug, "xai");
        assert_eq!(plan.provider.name.as_deref(), Some("xai"));
        assert_eq!(plan.provider.api_key.as_deref(), Some("xai-key"));
        assert_eq!(plan.provider.model.as_deref(), Some("grok-4.6"));
        assert!(plan.provider.protocol.is_none());
        assert!(plan.provider.base_url.is_none());
        assert!(plan.activate, "nothing usable is selected yet");
    }

    #[tokio::test]
    async fn a_custom_vendor_asks_for_the_endpoint_and_the_limits() {
        let ctx = context("");
        let mut args = args("my-proxy");
        args.model = Some("my-model".into());
        let mut p = Scripted::new()
            .texts(["https://proxy.example/v1", "1", "200k", "8k"])
            .secrets(["sk-gw"])
            .confirms([false, false]);
        let plan = plan(&mut p, &ctx, &args).await.unwrap();
        assert_eq!(
            plan.provider.base_url.as_deref(),
            Some("https://proxy.example/v1")
        );
        assert_eq!(plan.provider.protocol, Some(ProviderKind::Completions));
        assert_eq!(plan.provider.model.as_deref(), Some("my-model"));
        let metadata = &plan.provider.models["my-model"];
        assert_eq!(metadata.context_window, Some(200_000));
        assert_eq!(metadata.max_output_tokens, Some(8_000));
        assert_eq!(metadata.supports_tools, Some(false));
        assert_eq!(metadata.supports_vision, Some(false));
    }

    #[tokio::test]
    async fn a_scripted_custom_vendor_defaults_to_completions() {
        let ctx = context("");
        let mut args = args("my-proxy");
        args.base_url = Some("https://proxy.example/v1".into());
        args.model = Some("my-model".into());
        let mut p = Scripted::silent();
        let plan = plan(&mut p, &ctx, &args).await.unwrap();
        assert_eq!(plan.provider.protocol, Some(ProviderKind::Completions));
    }

    #[test]
    fn the_walk_picks_a_listed_model_by_number() {
        let provider = ProviderConfig {
            name: Some("my-proxy".into()),
            base_url: Some("https://proxy.example/v1".into()),
            ..Default::default()
        };
        let mut p = Scripted::new().texts(["2"]);
        let remote = vec!["my-model".to_string(), "my-model-mini".to_string()];
        let id = resolve_model(&mut p, &provider, &args("my-proxy"), &remote).unwrap();
        assert_eq!(id, "my-model-mini");
        assert!(p.said.iter().any(|line| line.contains("my-model")));
    }

    #[tokio::test]
    async fn adding_to_a_selected_provider_does_not_switch_the_model() {
        let ctx = context(
            "active = \"deepseek\"\n\n[providers.deepseek]\napi_key = \"k\"\nmodel = \"deepseek-v4-flash\"\n",
        );
        let mut args = args("deepseek");
        args.api_key_env = true;
        args.model = Some("deepseek-v4-pro".into());
        let mut p = Scripted::silent();
        let plan = plan(&mut p, &ctx, &args).await.unwrap();
        assert!(!plan.activate);
        assert_eq!(
            plan.provider.model.as_deref(),
            Some("deepseek-v4-flash"),
            "the selected model must not change"
        );
        assert!(
            plan.provider.models.is_empty(),
            "a shipped model with nothing to override is not declared: the \
             declaration would replace the catalog entry, not merge with it"
        );
    }

    #[tokio::test]
    async fn overriding_a_shipped_model_keeps_the_rest_of_its_catalog_entry() {
        let ctx = context("");
        let mut args = args("deepseek");
        args.api_key = Some("k".into());
        args.model = Some("deepseek-v4-flash".into());
        args.context_window = Some("500k".into());
        let mut p = Scripted::silent();
        let plan = plan(&mut p, &ctx, &args).await.unwrap();
        let metadata = &plan.provider.models["deepseek-v4-flash"];
        assert_eq!(metadata.context_window, Some(500_000));
        assert_eq!(metadata.max_output_tokens, Some(384_000));
        assert_eq!(metadata.supports_tools, Some(true));
    }

    #[tokio::test]
    async fn another_provider_keeps_its_own_default_model() {
        let ctx = context(
            "active = \"deepseek\"\n\n[providers.deepseek]\napi_key = \"k\"\nmodel = \"deepseek-v4-flash\"\n",
        );
        let mut args = args("xai");
        args.api_key = Some("xai-key".into());
        args.model = Some("grok-4.6".into());
        let mut p = Scripted::silent();
        let plan = plan(&mut p, &ctx, &args).await.unwrap();
        assert!(!plan.activate);
        assert_eq!(plan.provider.model.as_deref(), Some("grok-4.6"));
    }

    #[tokio::test]
    async fn a_script_must_pass_every_required_flag() {
        let ctx = context("");
        let mut p = Scripted::silent();
        let error = plan(&mut p, &ctx, &args("my-proxy")).await.unwrap_err();
        assert!(error.to_string().contains("--base-url"), "{error}");
    }

    #[tokio::test]
    async fn a_model_of_another_provider_is_rejected() {
        let ctx = context("");
        let mut args = args("my-proxy");
        args.base_url = Some("https://proxy.example/v1".into());
        args.model = Some("xai/grok-4.6".into());
        let mut p = Scripted::silent();
        let error = plan(&mut p, &ctx, &args).await.unwrap_err();
        assert!(
            error.to_string().contains("names provider 'xai'"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn the_added_provider_takes_over_when_nothing_usable_is_saved() {
        let ctx = context(
            "active = \"deepseek\"\n\n[providers.deepseek]\nmodel = \"deepseek-v4-flash\"\n",
        );
        if !ctx.config.needs_setup() {
            return;
        }
        let mut args = args("my-proxy");
        args.base_url = Some("https://proxy.example/v1".into());
        args.model = Some("my-model".into());
        args.api_key = Some("sk".into());
        let mut p = Scripted::silent();
        let plan = plan(&mut p, &ctx, &args).await.unwrap();
        assert!(
            plan.activate,
            "deepseek has no key, so it cannot stay active"
        );
    }

    #[test]
    fn a_bearer_prefix_is_stripped_from_the_key() {
        assert_eq!(check_key(" Bearer sk-1 "), "sk-1");
        assert_eq!(check_key("sk-2"), "sk-2");
    }

    #[test]
    fn token_suffixes_scale_and_overflow_fails() {
        assert_eq!(parse_tokens("200k").unwrap(), 200_000);
        assert_eq!(parse_tokens("1M").unwrap(), 1_000_000);
        assert_eq!(parse_tokens("8192").unwrap(), 8_192);
        assert!(parse_tokens("abc").is_err());
        assert!(parse_tokens("99999999999").is_err());
    }

    #[test]
    fn a_non_http_endpoint_is_rejected() {
        assert!(check_url("ftp://example.com").is_err());
        assert!(check_url("https://example.com/v1").is_ok());
    }

    #[tokio::test]
    async fn apply_writes_only_the_user_file() {
        let tmp = tempdir::TempDir::new("oven-add").unwrap();
        let user = tmp.path().join("config.toml");
        let ctx = Context {
            user_path: Some(user.clone()),
            project_path: tmp.path().join(".oven.toml"),
            config: AppConfig::empty(),
        };
        let plan = Plan {
            slug: "my-proxy".into(),
            id: "my-model".into(),
            provider: ProviderConfig {
                name: Some("my-proxy".into()),
                base_url: Some("https://proxy.example/v1".into()),
                api_key: Some("sk-gw".into()),
                model: Some("my-model".into()),
                ..Default::default()
            },
            activate: true,
        };
        let text = apply(&ctx, &plan).await.unwrap();
        assert!(
            text.contains("active model is now my-proxy/my-model"),
            "{text}"
        );
        let written = AppConfig::load_file(&user).await.unwrap().unwrap();
        assert_eq!(written.active_provider.name, "my-proxy");
        assert_eq!(
            written.providers["my-proxy"].model.as_deref(),
            Some("my-model")
        );
        assert!(!tmp.path().join(".oven.toml").exists());
    }
}

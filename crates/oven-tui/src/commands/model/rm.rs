//! `oven model rm`: drop a saved provider or one of its declared models.

use std::fmt::Write as _;

use clap::Args as ClapArgs;
use oven_app::AppError;
use oven_app::config::{AppConfig, ProviderConfig};
use oven_llm::canonical_vendor;

use super::Context;
use crate::commands::prompt::{Prompter, Stdin, required_text};

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    /// `<provider>` or `<provider>/<model>`; asked when omitted
    target: Option<String>,
    /// Do not ask before removing
    #[arg(long, short = 'y')]
    yes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    provider: String,
    model: Option<String>,
}

impl Target {
    fn label(&self) -> String {
        match &self.model {
            Some(id) => format!("{}/{id}", self.provider),
            None => format!("{} (the whole provider)", self.provider),
        }
    }
}

pub(crate) fn run(ctx: &Context, args: &Args) -> Result<String, AppError> {
    let mut prompter = Stdin::new();
    let target = match &args.target {
        Some(raw) => parse_target(raw)?,
        None => choose_target(&mut prompter, ctx)?,
    };
    if !args.yes && !prompter.confirm(&format!("remove {}?", target.label()), false) {
        return Ok("aborted; nothing removed".into());
    }
    remove(ctx, &target)
}

fn remove(ctx: &Context, target: &Target) -> Result<String, AppError> {
    let path = ctx.user_path()?;
    let mut config = AppConfig::load_file(path)?.unwrap_or_else(AppConfig::empty);
    if !config.providers.contains_key(&target.provider) {
        return Err(not_saved(ctx, &target.provider));
    }
    let was_active = config.active_provider.name == target.provider;
    let mut text = match &target.model {
        None => {
            let saved = &config.providers[&target.provider];
            let described = describe(saved);
            config.remove_provider(&target.provider);
            format!("removed {described} from {}", path.display())
        }
        Some(id) => {
            let Some(cleared) = config.remove_model(&target.provider, id) else {
                return Err(AppError::Runtime(format!(
                    "'{}/{id}' is not declared in {}; models oven ships or the endpoint serves cannot be removed",
                    target.provider,
                    path.display()
                )));
            };
            let mut line = format!("removed {}/{id} from {}", target.provider, path.display());
            if cleared {
                let fallback = config.providers[&target.provider].effective_model();
                let _ = write!(
                    line,
                    "\nit was the model in use; {} now uses {fallback}",
                    target.provider
                );
            }
            line
        }
    };
    AppConfig::save_at(path, &config)?;

    // A removed model already reported what its provider falls back to; only
    // a removed provider changes which one is selected.
    if was_active && target.model.is_none() {
        let _ = if config.active_provider.name.is_empty() {
            write!(text, "\nno provider is active; the next start opens /setup")
        } else {
            write!(
                text,
                "\nactive provider is now {}",
                config.active_provider.name
            )
        };
    }
    Ok(text)
}

/// A removed provider is worth summarizing by what went with it.
fn describe(provider: &ProviderConfig) -> String {
    let mut parts = Vec::new();
    if provider.api_key.is_some() {
        parts.push("api key".to_string());
    }
    match provider.models.len() {
        0 => {}
        1 => parts.push("1 model".to_string()),
        count => parts.push(format!("{count} models")),
    }
    let slug = provider.name.clone().unwrap_or_default();
    if parts.is_empty() {
        slug
    } else {
        format!("{slug} ({})", parts.join(", "))
    }
}

fn not_saved(ctx: &Context, slug: &str) -> AppError {
    if ctx.project_declares(slug) {
        return AppError::Runtime(format!(
            "'{slug}' is declared in {}, which this command does not write",
            ctx.project_path.display()
        ));
    }
    AppError::Runtime(format!("provider '{slug}' is not configured"))
}

fn choose_target(p: &mut dyn Prompter, ctx: &Context) -> Result<Target, AppError> {
    let mut items: Vec<(String, Target)> = Vec::new();
    for (slug, provider) in &ctx.config.providers {
        items.push((
            format!("{slug} (provider)"),
            Target {
                provider: slug.clone(),
                model: None,
            },
        ));
        for id in provider.models.keys() {
            items.push((
                format!("{slug}/{id} (model)"),
                Target {
                    provider: slug.clone(),
                    model: Some(id.clone()),
                },
            ));
        }
        if let Some(id) = provider.model.clone()
            && !provider.models.contains_key(&id)
        {
            items.push((
                format!("{slug}/{id} (model in use)"),
                Target {
                    provider: slug.clone(),
                    model: Some(id),
                },
            ));
        }
    }
    if items.is_empty() {
        return Err(AppError::Runtime("no providers are configured".into()));
    }
    for (index, (label, _)) in items.iter().enumerate() {
        p.say(&format!("  {}) {label}", index + 1));
    }
    let answer = required_text(p, "remove (number or target)", None, "<provider>[/<model>]")?;
    match answer.parse::<usize>() {
        Ok(index) if (1..=items.len()).contains(&index) => Ok(items[index - 1].1.clone()),
        _ => parse_target(&answer),
    }
}

fn parse_target(raw: &str) -> Result<Target, AppError> {
    let raw = raw.trim();
    match raw.split_once('/') {
        Some((provider, id)) if !provider.is_empty() && !id.is_empty() => Ok(Target {
            provider: canonical_vendor(provider),
            model: Some(id.to_string()),
        }),
        None if !raw.is_empty() => Ok(Target {
            provider: canonical_vendor(raw),
            model: None,
        }),
        _ => Err(AppError::Runtime(format!(
            "invalid target '{raw}'; expected <provider> or <provider>/<model>"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAVED: &str = "active = \"xai\"\n\n[providers.xai]\napi_key = \"x\"\nmodel = \"grok-4.6\"\n\n[providers.xai.models.\"grok-4.6\"]\ncontext_window = 256000\n\n[providers.deepseek]\napi_key = \"d\"\nmodel = \"deepseek-v4-flash\"\n";

    fn setup(config: &str) -> (tempdir::TempDir, Context) {
        let tmp = tempdir::TempDir::new("oven-rm").unwrap();
        let user = tmp.path().join("config.toml");
        std::fs::write(&user, config).unwrap();
        let ctx = Context {
            user_path: Some(user),
            project_path: tmp.path().join(".oven.toml"),
            config: toml::from_str(config).unwrap(),
        };
        (tmp, ctx)
    }

    #[test]
    fn removing_a_provider_reselects_and_reports_the_new_active() {
        let (_tmp, ctx) = setup(SAVED);
        let text = remove(&ctx, &parse_target("xai").unwrap()).unwrap();
        assert!(text.contains("removed xai (api key, 1 model)"), "{text}");
        assert!(text.contains("active provider is now deepseek"), "{text}");
        let written = AppConfig::load_file(ctx.user_path().unwrap())
            .unwrap()
            .unwrap();
        assert!(!written.providers.contains_key("xai"));
        assert_eq!(written.active_provider.name, "deepseek");
    }

    #[test]
    fn removing_a_declared_model_reports_the_fallback() {
        let (_tmp, ctx) = setup(SAVED);
        let text = remove(&ctx, &parse_target("xai/grok-4.6").unwrap()).unwrap();
        assert!(text.contains("removed xai/grok-4.6"), "{text}");
        assert!(text.contains("now uses"), "{text}");
        let written = AppConfig::load_file(ctx.user_path().unwrap())
            .unwrap()
            .unwrap();
        assert!(written.providers["xai"].model.is_none());
        assert!(written.providers["xai"].models.is_empty());
    }

    #[test]
    fn removing_a_model_that_is_not_declared_errors() {
        let (_tmp, ctx) = setup(SAVED);
        let error = remove(&ctx, &parse_target("xai/grok-4").unwrap()).unwrap_err();
        assert!(error.to_string().contains("not declared"), "{error}");
    }

    #[test]
    fn removing_an_unknown_provider_errors() {
        let (_tmp, ctx) = setup(SAVED);
        let error = remove(&ctx, &parse_target("moonshot").unwrap()).unwrap_err();
        assert!(error.to_string().contains("not configured"), "{error}");
    }

    #[test]
    fn a_project_only_provider_is_reported_rather_than_removed() {
        let (_tmp, ctx) = setup(SAVED);
        std::fs::write(&ctx.project_path, "[providers.moonshot]\napi_key = \"m\"\n").unwrap();
        let error = remove(&ctx, &parse_target("moonshot").unwrap()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("declared in"), "{message}");
        assert!(message.contains("does not write"), "{message}");
    }

    #[test]
    fn a_grok_alias_names_the_same_provider() {
        assert_eq!(parse_target("grok").unwrap().provider, "xai");
        assert_eq!(
            parse_target("xai/grok-4.6").unwrap(),
            Target {
                provider: "xai".into(),
                model: Some("grok-4.6".into()),
            }
        );
    }

    #[test]
    fn a_malformed_target_is_rejected() {
        for raw in ["", "/", "xai/", "/model", "  "] {
            assert!(parse_target(raw).is_err(), "{raw}");
        }
    }
}

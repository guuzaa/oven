//! `oven model ls`: what is configured, and what the shipped catalog adds.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::time::Duration;

use clap::Args as ClapArgs;
use oven_app::AppError;
use oven_app::config::{ModelMetadata, ProviderConfig};
use oven_app::{provider_catalog, provider_models};
use oven_llm::{ModelCapabilities, ModelInfo, ProviderName, canonical_vendor};
use unicode_width::UnicodeWidthStr;

use super::Context;
use crate::platform::style::{Ink, Palette};

/// Endpoint ids, or why the endpoint could not be asked, per provider.
type Remote = BTreeMap<String, Result<Vec<String>, String>>;

/// How long a `--refresh` request may take before it is reported as failed.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

const TITLE: &str = "oven model ls";
const CONFIG_LABEL: &str = "config:";
const ACTIVE_LABEL: &str = "active:";
const NO_ACTIVE: &str = "(none)";
const NO_PROVIDERS: &str = "no providers configured";
const ADD_HINT: &str = "run `oven model add` to add one";
const UNKNOWN: &str = "-";
const FIELD_SEPARATOR: &str = " · ";
const INDENT: &str = "  ";
const COLUMN_GAP: usize = 2;
const MARKER_WIDTH: usize = 1;
const RULE: &str = "─";

#[derive(Debug, Clone, Default, ClapArgs)]
pub(crate) struct Args {
    /// Only show this provider
    provider: Option<String>,
    /// Ask each endpoint which models it serves
    #[arg(long)]
    refresh: bool,
}

pub(crate) async fn run(ctx: &Context, args: &Args) -> Result<String, AppError> {
    let only = match &args.provider {
        Some(raw) => {
            let slug = canonical_vendor(raw);
            if !ctx.config.providers.contains_key(&slug) {
                return Err(AppError::Runtime(format!("unknown provider '{raw}'")));
            }
            Some(slug)
        }
        None => None,
    };
    let remote = if args.refresh {
        refresh(ctx, only.as_deref()).await
    } else {
        BTreeMap::new()
    };
    Ok(render(ctx, only.as_deref(), &remote, Palette::detect()))
}

/// One unreachable gateway must not fail the whole listing.
async fn refresh(ctx: &Context, only: Option<&str>) -> Remote {
    let timeout = ctx.config.request_timeout().min(REFRESH_TIMEOUT);
    let mut remote = BTreeMap::new();
    for (slug, provider) in &ctx.config.providers {
        if only.is_some_and(|want| want != slug) {
            continue;
        }
        let ids = match provider_models(provider, timeout).await {
            Ok(models) => Ok(models.into_iter().map(|model| model.id).collect()),
            Err(error) => Err(error.to_string()),
        };
        remote.insert(slug.clone(), ids);
    }
    remote
}

fn render(ctx: &Context, only: Option<&str>, remote: &Remote, ink: Palette) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", ink.paint(Ink::Bold, TITLE));
    let _ = writeln!(
        out,
        "{INDENT}{} {}",
        ink.paint(Ink::Dim, CONFIG_LABEL),
        sources(ctx)
    );
    let active = match ctx.config.active_provider.name.as_str() {
        "" => NO_ACTIVE.to_string(),
        name => ink.paint(Ink::Green, name),
    };
    let _ = writeln!(
        out,
        "{INDENT}{} {active}",
        ink.paint(Ink::Dim, ACTIVE_LABEL)
    );
    if ctx.config.providers.is_empty() {
        let _ = writeln!(out, "\n{NO_PROVIDERS}; {ADD_HINT}");
        return out;
    }
    for (slug, provider) in &ctx.config.providers {
        if only.is_some_and(|want| want != slug) {
            continue;
        }
        let _ = writeln!(out, "\n{}", header(slug, provider, ink));
        if let Some(Err(error)) = remote.get(slug) {
            let _ = writeln!(
                out,
                "{INDENT}{} model list unavailable: {error}",
                ink.paint(Ink::Red, "!")
            );
        }
        let reported = remote.get(slug).and_then(|ids| ids.as_ref().ok());
        let selected = ctx.config.active_provider.name == *slug;
        out.push_str(&table(ink, &rows(provider, selected, reported)));
    }
    out
}

fn sources(ctx: &Context) -> String {
    let paths: Vec<String> = [ctx.user_path.as_deref(), Some(ctx.project_path.as_path())]
        .into_iter()
        .flatten()
        .filter(|path| path.exists())
        .map(|path| path.display().to_string())
        .collect();
    if paths.is_empty() {
        "(none yet)".to_string()
    } else {
        paths.join(" + ")
    }
}

fn header(slug: &str, provider: &ProviderConfig, ink: Palette) -> String {
    let vendor = provider.effective_provider_name();
    let kind = if matches!(vendor, ProviderName::Custom(_)) {
        "custom"
    } else {
        "preset"
    };
    let protocol = provider
        .protocol
        .or_else(|| vendor.default_protocol())
        .map_or_else(|| UNKNOWN.to_string(), |kind| kind.to_string());
    let base_url = provider
        .effective_base_url()
        .or_else(|| {
            provider
                .name
                .as_deref()
                .and_then(ProviderConfig::suggested_base_url)
                .map(str::to_string)
        })
        .unwrap_or_else(|| UNKNOWN.to_string());
    let (key, key_ink) = match provider.effective_api_key().is_empty() {
        true => ("no key", Ink::Yellow),
        false => ("key set", Ink::Green),
    };
    let fields = [
        ink.paint(Ink::Dim, kind),
        ink.paint(Ink::Dim, &protocol),
        ink.paint(Ink::Dim, &base_url),
        ink.paint(key_ink, key),
    ];
    format!(
        "{}  {}",
        ink.paint(Ink::Bold, slug),
        fields.join(&ink.paint(Ink::Dim, FIELD_SEPARATOR))
    )
}

/// Every id worth showing for one provider: the model the provider would use,
/// then what the config declares, then what the catalog ships, then what the
/// endpoint reports. The first source to name an id owns its row.
fn rows(provider: &ProviderConfig, selected: bool, remote: Option<&Vec<String>>) -> Vec<Row> {
    // The catalog and an endpoint list arrive unordered; a listing that
    // reshuffles between runs is worse than useless.
    let mut catalog = provider_catalog(provider);
    catalog.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    let mut reported: Vec<&str> = remote.into_iter().flatten().map(String::as_str).collect();
    reported.sort_unstable();
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();

    let in_use = wire_id(&provider.effective_model()).to_string();
    let facts = declared_facts(&in_use, provider)
        .or_else(|| {
            catalog
                .iter()
                .find(|info| info.id == in_use)
                .map(Facts::shipped)
        })
        .unwrap_or_default();
    seen.insert(in_use.clone());
    // One provider's model is the one in use; every other provider's is only
    // what it would fall back to.
    let source = match selected {
        true => Source::Active,
        false => Source::Default,
    };
    rows.push(Row::new(source, in_use, facts));

    for (id, metadata) in provider.effective_models() {
        if seen.insert(id.to_string()) {
            rows.push(Row::new(
                Source::Declared,
                id.to_string(),
                Facts::declared(&metadata),
            ));
        }
    }
    for info in &catalog {
        if seen.insert(info.id.clone()) {
            rows.push(Row::new(
                Source::Shipped,
                info.id.clone(),
                Facts::shipped(info),
            ));
        }
    }
    for id in reported {
        if seen.insert(id.to_string()) {
            rows.push(Row::new(Source::Endpoint, id.to_string(), Facts::default()));
        }
    }
    rows
}

fn declared_facts(id: &str, provider: &ProviderConfig) -> Option<Facts> {
    provider
        .effective_models()
        .find(|(name, _)| *name == id)
        .map(|(_, metadata)| Facts::declared(&metadata))
}

/// The wire id inside a slug, variant included.
fn wire_id(slug: &str) -> &str {
    slug.split_once('/').map_or(slug, |(_, wire)| wire)
}

/// One column of the model table: the narrowest it can be, and which way its
/// values sit so magnitudes can be compared at a glance.
#[derive(Clone, Copy)]
struct Column {
    title: &'static str,
    right: bool,
}

const COLUMNS: [Column; 5] = [
    Column {
        title: "MODEL",
        right: false,
    },
    Column {
        title: "CONTEXT",
        right: true,
    },
    Column {
        title: "OUTPUT",
        right: true,
    },
    Column {
        title: "CAPABILITIES",
        right: false,
    },
    Column {
        title: "SOURCE",
        right: false,
    },
];

struct Row {
    id: String,
    context: String,
    output: String,
    caps: String,
    source: Source,
}

impl Row {
    fn new(source: Source, id: String, facts: Facts) -> Self {
        Self {
            id,
            context: facts.context(),
            output: facts.output(),
            caps: facts.caps(),
            source,
        }
    }

    /// Every cell with the ink it is painted in: the columns that name a model
    /// stay plain, the ones that qualify it recede, and the source of the row
    /// that is in use is called out.
    fn cells(&self) -> [(&str, Ink); COLUMNS.len()] {
        [
            (&self.id, Ink::Plain),
            (&self.context, Ink::Plain),
            (&self.output, Ink::Plain),
            (&self.caps, Ink::Dim),
            (self.source.label(), self.source.label_ink()),
        ]
    }
}

/// Where a listed id came from, which is also how its row is marked.
#[derive(Clone, Copy)]
enum Source {
    Active,
    Default,
    Declared,
    Shipped,
    Endpoint,
}

impl Source {
    const fn marker(self) -> (&'static str, Ink) {
        match self {
            Self::Active => ("*", Ink::Green),
            Self::Default | Self::Declared | Self::Shipped | Self::Endpoint => (" ", Ink::Plain),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Default => "default",
            Self::Declared => "declared",
            Self::Shipped => "shipped",
            Self::Endpoint => "endpoint",
        }
    }

    const fn label_ink(self) -> Ink {
        match self {
            Self::Active => Ink::Green,
            Self::Default | Self::Declared | Self::Shipped | Self::Endpoint => Ink::Dim,
        }
    }
}

/// What a listed model can do. Unset limits stay unknown and unset
/// capabilities default to supported, matching how the config is read.
#[derive(Default)]
struct Facts {
    context_window: Option<u32>,
    max_output_tokens: Option<u32>,
    tools: bool,
    vision: bool,
    streaming: bool,
}

impl Facts {
    fn declared(metadata: &ModelMetadata) -> Self {
        let caps = ModelCapabilities::supported().with_overrides(
            metadata.supports_vision,
            metadata.supports_tools,
            metadata.supports_streaming,
            metadata.supports_system_prompt,
        );
        Self {
            context_window: metadata.context_window,
            max_output_tokens: metadata.max_output_tokens,
            tools: caps.supports_tools,
            vision: caps.supports_vision,
            streaming: caps.supports_streaming,
        }
    }

    fn shipped(info: &ModelInfo) -> Self {
        Self {
            context_window: non_zero(info.context_window),
            max_output_tokens: non_zero(info.max_output_tokens),
            tools: info.capabilities.supports_tools,
            vision: info.capabilities.supports_vision,
            streaming: info.capabilities.supports_streaming,
        }
    }

    fn context(&self) -> String {
        tokens(self.context_window)
    }

    fn output(&self) -> String {
        tokens(self.max_output_tokens)
    }

    fn caps(&self) -> String {
        let mut parts = Vec::new();
        for (supported, label) in [
            (self.tools, "tools"),
            (self.vision, "vision"),
            (self.streaming, "stream"),
        ] {
            if supported {
                parts.push(label);
            }
        }
        if parts.is_empty() {
            UNKNOWN.to_string()
        } else {
            parts.join(" ")
        }
    }
}

fn table(ink: Palette, rows: &[Row]) -> String {
    let widths = widths(rows);
    let span = MARKER_WIDTH + 1 + widths.iter().sum::<usize>() + COLUMN_GAP * (COLUMNS.len() - 1);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{INDENT}{}{}",
        " ".repeat(MARKER_WIDTH + 1),
        line(ink, COLUMNS.map(|column| (column.title, Ink::Dim)), &widths)
    );
    let _ = writeln!(out, "{INDENT}{}", ink.paint(Ink::Dim, &RULE.repeat(span)));
    for row in rows {
        let (marker, marker_ink) = row.source.marker();
        let _ = writeln!(
            out,
            "{INDENT}{} {}",
            ink.paint(marker_ink, marker),
            line(ink, row.cells(), &widths)
        );
    }
    out
}

fn widths(rows: &[Row]) -> [usize; COLUMNS.len()] {
    let mut widths = COLUMNS.map(|column| column.title.width());
    for row in rows {
        for (width, (cell, _)) in widths.iter_mut().zip(row.cells()) {
            *width = (*width).max(cell.width());
        }
    }
    widths
}

fn line(
    ink: Palette,
    cells: [(&str, Ink); COLUMNS.len()],
    widths: &[usize; COLUMNS.len()],
) -> String {
    let gap = " ".repeat(COLUMN_GAP);
    let last = COLUMNS.len() - 1;
    cells
        .iter()
        .zip(widths)
        .enumerate()
        .map(|(index, ((text, cell_ink), width))| match index == last {
            true => ink.paint(*cell_ink, text),
            false => ink.paint(*cell_ink, &pad(text, *width, COLUMNS[index].right)),
        })
        .collect::<Vec<String>>()
        .join(&gap)
}

fn pad(text: &str, width: usize, right: bool) -> String {
    let fill = " ".repeat(width.saturating_sub(text.width()));
    match right {
        true => format!("{fill}{text}"),
        false => format!("{text}{fill}"),
    }
}

fn non_zero(value: u32) -> Option<u32> {
    (value > 0).then_some(value)
}

fn tokens(value: Option<u32>) -> String {
    match value {
        None => UNKNOWN.to_string(),
        Some(value) if value >= 1_000_000 => format!("{:.1}M", f64::from(value) / 1_000_000.0),
        Some(value) if value >= 1_000 => format!("{}k", value / 1_000),
        Some(value) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use oven_app::config::AppConfig;

    use super::*;

    fn context(config: &str) -> Context {
        Context {
            user_path: Some(PathBuf::from("/tmp/does-not-exist/config.toml")),
            project_path: PathBuf::from("/tmp/does-not-exist/.oven.toml"),
            config: toml::from_str(config).unwrap(),
        }
    }

    fn plain(ctx: &Context) -> String {
        render(ctx, None, &Remote::new(), Palette::default())
    }

    fn row_for<'a>(text: &'a str, id: &str) -> &'a str {
        text.lines()
            .find(|line| line.contains(id))
            .unwrap_or_else(|| panic!("no row for {id} in {text}"))
    }

    #[test]
    fn lists_declared_models_and_the_shipped_catalog() {
        let ctx = context(
            "active = \"deepseek\"\n\n[providers.deepseek]\napi_key = \"k\"\nmodel = \"deepseek-v4-flash\"\n",
        );
        let text = plain(&ctx);
        assert!(text.contains("active: deepseek"), "{text}");
        assert!(text.contains("MODEL"), "{text}");
        assert!(text.contains("CAPABILITIES"), "{text}");
        assert!(row_for(&text, "deepseek-v4-flash").starts_with("  * deepseek-v4-flash"));
        assert!(text.contains("deepseek-v4-pro"), "{text}");
        assert!(text.contains("shipped"), "{text}");
    }

    #[test]
    fn declared_metadata_overrides_the_catalog() {
        let ctx = context(
            "active = \"deepseek\"\n\n[providers.deepseek]\napi_key = \"k\"\nmodel = \"deepseek-v4-flash\"\n\n[providers.deepseek.models.\"deepseek-v4-flash\"]\ncontext_window = 42000\nmax_output_tokens = 8192\n",
        );
        let text = plain(&ctx);
        let row = row_for(&text, "deepseek-v4-flash");
        assert!(row.contains("42k"), "{row}");
        assert!(row.contains("8k"), "{row}");
    }

    #[test]
    fn a_custom_vendor_shows_its_declared_models() {
        let ctx = context(
            "active = \"myproxy\"\n\n[providers.myproxy]\nbase_url = \"https://proxy.example/v1\"\napi_key = \"k\"\nmodel = \"my-model\"\n\n[providers.myproxy.models.\"my-model\"]\ncontext_window = 200000\nmax_output_tokens = 8192\nsupports_vision = false\n",
        );
        let text = plain(&ctx);
        assert!(text.contains("custom · completions"), "{text}");
        let row = row_for(&text, "my-model");
        assert!(row.starts_with("  * my-model"), "{row}");
        assert!(row.contains("200k"), "{row}");
        assert!(row.contains("8k"), "{row}");
        assert!(row.contains("tools stream"), "{row}");
        assert!(!text.contains("vision"), "{text}");
    }

    #[test]
    fn an_endpoint_id_is_listed_after_the_shipped_one() {
        let ctx = context(
            "active = \"openai\"\n\n[providers.openai]\napi_key = \"k\"\nmodel = \"gpt-5.6-terra\"\n",
        );
        let mut remote = Remote::new();
        remote.insert("openai".to_string(), Ok(vec!["gpt-5.6-sol".to_string()]));
        let text = render(&ctx, None, &remote, Palette::default());
        assert!(text.contains("* gpt-5.6-terra"), "{text}");
        assert!(text.contains("gpt-5.6-sol"), "{text}");
        assert!(text.contains("endpoint"), "{text}");
        assert!(text.contains("preset · responses"), "{text}");
    }

    #[test]
    fn endpoint_ids_keep_the_same_order_between_runs() {
        let ctx = context(
            "active = \"myproxy\"\n\n[providers.myproxy]\nbase_url = \"https://proxy.example/v1\"\napi_key = \"k\"\nmodel = \"my-model\"\n",
        );
        let mut remote = Remote::new();
        remote.insert(
            "myproxy".to_string(),
            Ok(vec!["zeta".to_string(), "alpha".to_string()]),
        );
        let text = render(&ctx, None, &remote, Palette::default());
        let ids = text
            .lines()
            .filter(|line| line.contains("endpoint"))
            .map(|line| line.split_whitespace().next().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["alpha", "zeta"], "{text}");
    }

    #[test]
    fn an_unreachable_endpoint_is_reported_without_losing_the_list() {
        let ctx = context(
            "active = \"myproxy\"\n\n[providers.myproxy]\nbase_url = \"https://proxy.example/v1\"\napi_key = \"k\"\n",
        );
        let mut remote = Remote::new();
        remote.insert("myproxy".to_string(), Err("connection refused".to_string()));
        let text = render(&ctx, None, &remote, Palette::default());
        assert!(
            text.contains("! model list unavailable: connection refused"),
            "{text}"
        );
        assert!(text.contains("* deepseek-v4-flash"), "{text}");
    }

    #[test]
    fn an_empty_config_points_at_the_add_command() {
        let ctx = Context {
            user_path: None,
            project_path: PathBuf::from("/tmp/does-not-exist/.oven.toml"),
            config: AppConfig::empty(),
        };
        let text = plain(&ctx);
        assert!(text.contains("active: (none)"), "{text}");
        assert!(text.contains("oven model add"), "{text}");
    }

    #[test]
    fn columns_line_up_across_rows_of_different_id_length() {
        let ctx = context(
            "active = \"deepseek\"\n\n[providers.deepseek]\napi_key = \"k\"\nmodel = \"deepseek-v4-flash\"\n",
        );
        let text = plain(&ctx);
        let capabilities = text
            .lines()
            .filter(|line| line.contains("tools"))
            .map(|line| line.find("tools").unwrap())
            .collect::<Vec<_>>();
        assert!(capabilities.len() > 1, "{text}");
        assert!(
            capabilities.windows(2).all(|pair| pair[0] == pair[1]),
            "{text}"
        );
        assert!(!text.lines().any(|line| line.ends_with(' ')), "{text}");
    }

    #[test]
    fn padding_counts_display_width() {
        assert_eq!(pad("ab", 4, false), "ab  ");
        assert_eq!(pad("ab", 4, true), "  ab");
        assert_eq!(pad("模型", 4, false), "模型");
    }

    #[test]
    fn tokens_shrink_to_a_readable_unit() {
        assert_eq!(tokens(None), UNKNOWN);
        assert_eq!(tokens(Some(384_000)), "384k");
        assert_eq!(tokens(Some(1_000_000)), "1.0M");
    }

    #[tokio::test]
    async fn an_unknown_provider_is_an_error() {
        let ctx = context("active = \"deepseek\"\n\n[providers.deepseek]\napi_key = \"k\"\n");
        let args = Args {
            provider: Some("nope".into()),
            refresh: false,
        };
        let error = run(&ctx, &args).await.unwrap_err();
        assert!(error.to_string().contains("unknown provider"), "{error}");
        let args = Args {
            provider: Some("grok".into()),
            refresh: false,
        };
        // `grok` canonicalizes onto a provider that is not configured either.
        assert!(run(&ctx, &args).await.is_err());
    }
}

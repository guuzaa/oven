use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::mem;
use std::path::{Path, PathBuf};
use std::time::Duration;

use oven_agent::DEFAULT_MAX_ITERS;
use oven_llm::{ModelId, ModelInfo, ProviderKind, ProviderName, ReasoningEffort, canonical_vendor};
use serde::de::{Error as _, MapAccess, Visitor};
use serde::ser::SerializeMap as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

pub mod mcp;

pub use mcp::McpServerConfig;

const DEFAULT_SUBAGENT_CONCURRENT: usize = 4;
const DEFAULT_SUBAGENT_ITERS: usize = 60;

/// Stands in for the API key of a printed config preview.
const REDACTED_KEY: &str = "<redacted>";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("read config {0}: {1}")]
    Read(PathBuf, #[source] std::io::Error),
    #[error("parse config {0}: {1}")]
    Parse(PathBuf, #[source] toml::de::Error),
    #[error("write config {0}: {1}")]
    Write(PathBuf, #[source] std::io::Error),
    #[error("serialize config {0}: {1}")]
    Serialize(PathBuf, #[source] toml::ser::Error),
    #[error("unknown provider {0}")]
    InvalidProvider(String),
}

/// LLM provider configuration. All fields optional so users can override just
/// what they need; environment variables can supply the rest at runtime.
///
/// The canonical slug is the `[providers.<slug>]` table key, so `name` is only
/// ever set in memory (and accepted on read for hand-written files) and never
/// written back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderConfig {
    /// Canonical slug; also the `[providers.<slug>]` table key.
    pub name: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    /// Wire protocol for unknown vendors only. Known vendors ignore this.
    pub protocol: Option<ProviderKind>,
    pub api_key: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Metadata every entry under `models` inherits where it leaves a field
    /// unset, so a vendor whose models share one window declares it once. It
    /// sits directly in the provider table.
    pub metadata: ModelMetadata,
    /// Per-model metadata declared in `[providers.<slug>.models.<wire-id>]`
    /// tables. Overrides the static catalog when the ids collide, and is the
    /// only source of window sizes for custom vendors.
    pub models: BTreeMap<String, ModelMetadata>,
}

/// A provider table without the metadata keys, used to fill the rest: TOML
/// cannot hand a [`serde::flatten`]ed field its keys — that needs
/// `deserialize_any`, which buffers the table as one value — so the table is
/// read once for the plain fields and once for the metadata keys, which
/// [`ModelMetadata`] picks out while ignoring the rest.
#[derive(Deserialize)]
struct ProviderRest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    protocol: Option<ProviderKind>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, deserialize_with = "models_from_toml")]
    models: BTreeMap<String, ModelMetadata>,
}

/// Entries keyed by wire id; legacy array-of-tables files carry the id inside
/// the entry instead.
#[derive(Deserialize)]
#[serde(untagged)]
enum ModelsField {
    Map(BTreeMap<String, ModelMetadata>),
    Array(Vec<ModelEntry>),
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(flatten)]
    fields: ModelMetadata,
}

/// One model's metadata, kept id-less so declaring it is a quoted table key
/// (`[providers.x.models."gpt-4.1"]`) rather than a repeated `id` field. The
/// provider table's own flat metadata keys share it. Unset limits stay unknown
/// (skipped by request validation) and unset capabilities default to supported,
/// so declaring a model keeps the passthrough behaviour of leaving it
/// undeclared.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub supports_system_prompt: Option<bool>,
    pub supports_tools: Option<bool>,
    pub supports_streaming: Option<bool>,
    pub supports_vision: Option<bool>,
}

impl ModelMetadata {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Every field of a catalog entry, so declaring a shipped model to change
    /// one value does not drop the rest: a declared entry replaces the
    /// catalog's, it does not merge with it.
    pub fn from_info(info: &ModelInfo) -> Self {
        Self {
            context_window: (info.context_window > 0).then_some(info.context_window),
            max_output_tokens: (info.max_output_tokens > 0).then_some(info.max_output_tokens),
            supports_system_prompt: Some(info.capabilities.supports_system_prompt),
            supports_tools: Some(info.capabilities.supports_tools),
            supports_streaming: Some(info.capabilities.supports_streaming),
            supports_vision: Some(info.capabilities.supports_vision),
        }
    }

    /// Overlay the fields `overlay` sets onto `self`.
    pub fn merge_fields(&mut self, overlay: &Self) {
        if overlay.context_window.is_some() {
            self.context_window = overlay.context_window;
        }
        if overlay.max_output_tokens.is_some() {
            self.max_output_tokens = overlay.max_output_tokens;
        }
        if overlay.supports_system_prompt.is_some() {
            self.supports_system_prompt = overlay.supports_system_prompt;
        }
        if overlay.supports_tools.is_some() {
            self.supports_tools = overlay.supports_tools;
        }
        if overlay.supports_streaming.is_some() {
            self.supports_streaming = overlay.supports_streaming;
        }
        if overlay.supports_vision.is_some() {
            self.supports_vision = overlay.supports_vision;
        }
    }

    fn fill_missing(&mut self, src: &Self) {
        self.context_window = self.context_window.or(src.context_window);
        self.max_output_tokens = self.max_output_tokens.or(src.max_output_tokens);
        self.supports_system_prompt = self.supports_system_prompt.or(src.supports_system_prompt);
        self.supports_tools = self.supports_tools.or(src.supports_tools);
        self.supports_streaming = self.supports_streaming.or(src.supports_streaming);
        self.supports_vision = self.supports_vision.or(src.supports_vision);
    }
}

fn models_from_toml<'de, D: Deserializer<'de>>(
    de: D,
) -> Result<BTreeMap<String, ModelMetadata>, D::Error> {
    Ok(match ModelsField::deserialize(de)? {
        ModelsField::Map(entries) => entries,
        ModelsField::Array(entries) => entries
            .into_iter()
            .map(|entry| (entry.id, entry.fields))
            .collect(),
    })
}

impl<'de> Deserialize<'de> for ProviderConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ProviderTable)
    }
}

struct ProviderTable;

impl<'de> Visitor<'de> for ProviderTable {
    type Value = ProviderConfig;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a provider table")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut table = toml::Table::new();
        while let Some(key) = map.next_key::<String>()? {
            table.insert(key, map.next_value()?);
        }
        let value = toml::Value::Table(table);
        let rest: ProviderRest = value.clone().try_into().map_err(A::Error::custom)?;
        let metadata: ModelMetadata = value.try_into().map_err(A::Error::custom)?;
        Ok(ProviderConfig {
            name: rest.name,
            model: rest.model,
            base_url: rest.base_url,
            protocol: rest.protocol,
            api_key: rest.api_key,
            reasoning_effort: rest.reasoning_effort,
            metadata,
            models: rest.models,
        })
    }
}

/// Writes only what the file stores: `name` is the table key, the metadata
/// keys sit flat in the provider table, and each model's id is its own table
/// key.
impl Serialize for ProviderConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("model", &self.model)?;
        map.serialize_entry("base_url", &self.base_url)?;
        map.serialize_entry("protocol", &self.protocol)?;
        map.serialize_entry("api_key", &self.api_key)?;
        map.serialize_entry("reasoning_effort", &self.reasoning_effort)?;
        map.serialize_entry("context_window", &self.metadata.context_window)?;
        map.serialize_entry("max_output_tokens", &self.metadata.max_output_tokens)?;
        map.serialize_entry(
            "supports_system_prompt",
            &self.metadata.supports_system_prompt,
        )?;
        map.serialize_entry("supports_tools", &self.metadata.supports_tools)?;
        map.serialize_entry("supports_streaming", &self.metadata.supports_streaming)?;
        map.serialize_entry("supports_vision", &self.metadata.supports_vision)?;
        if !self.models.is_empty() {
            let models = ModelsRef {
                models: &self.models,
                inheriting: !self.metadata.is_empty(),
            };
            map.serialize_entry("models", &models)?;
        }
        map.end()
    }
}

impl ProviderConfig {
    /// Model used when neither `model` nor `OVEN_MODEL` is set.
    pub const DEFAULT_MODEL: &str = "deepseek-v4-flash";

    /// Canonicalize `name` aliases and store `model` as a wire id (no vendor).
    pub fn normalize(&mut self) {
        if let Some(name) = self.name.take() {
            self.name = canonical_name(&name);
        }
        if let Some(model) = self.model.take() {
            self.model = Some(wire_model(&model));
        }
        for (id, metadata) in mem::take(&mut self.models) {
            self.models.entry(wire_model(&id)).or_insert(metadata);
        }
        if self.protocol.is_some() && !self.is_custom_vendor() {
            self.protocol = None;
        }
    }

    pub fn is_custom_vendor(&self) -> bool {
        self.name
            .as_deref()
            .and_then(canonical_name)
            .is_none_or(|name| matches!(ProviderName::from(name.as_str()), ProviderName::Custom(_)))
    }

    /// The effective model slug: `model` config wins, then the `OVEN_MODEL` env
    /// var, then the preset for `name`, then [`ProviderConfig::DEFAULT_MODEL`].
    /// Wire ids are joined with `name` (or `deepseek` for the builtin default).
    pub fn effective_model(&self) -> String {
        if let Some(raw) = self.model.clone().or_else(|| env::var("OVEN_MODEL").ok()) {
            return qualify_model(&raw, self.name.as_deref());
        }
        if let Some(name) = self.name.as_deref()
            && let Some(suggested) = Self::suggested_model(name)
        {
            return qualify_model(suggested, Some(name));
        }
        qualify_model(
            Self::DEFAULT_MODEL,
            self.name.as_deref().or(Some("deepseek")),
        )
    }

    /// Endpoint preset for a known vendor; `oven-llm` owns the one table.
    pub fn suggested_base_url(name: &str) -> Option<&'static str> {
        ProviderName::from(name).base_url()
    }

    pub fn suggested_model(name: &str) -> Option<&'static str> {
        ProviderName::from(name).default_model()
    }

    /// Fill `base_url` and `model` from [`name`](Self::name) presets.
    pub fn apply_name_presets(&mut self) {
        self.normalize();
        let Some(name) = self.name.as_deref() else {
            return;
        };
        if let Some(url) = Self::suggested_base_url(name) {
            self.base_url = Some(url.to_string());
        }
        if let Some(model) = Self::suggested_model(name) {
            self.model = Some(model.to_string());
        }
    }

    /// Provider from the canonical `name`, or the vendor segment of the model slug.
    pub fn effective_provider_name(&self) -> ProviderName {
        if let Some(name) = self.name.as_deref().and_then(canonical_name) {
            return ProviderName::from(name.as_str());
        }
        match ModelId::from(self.effective_model().as_str()).vendor() {
            Some(vendor) => ProviderName::from(vendor),
            None => ProviderName::Custom("unknown".into()),
        }
    }

    pub fn parse_protocol(raw: &str) -> Option<ProviderKind> {
        match raw.to_ascii_lowercase().as_str() {
            "completions" => Some(ProviderKind::Completions),
            "responses" => Some(ProviderKind::Responses),
            "messages" => Some(ProviderKind::Messages),
            _ => None,
        }
    }

    pub fn parse_effort(raw: &str) -> Option<ReasoningEffort> {
        match raw.to_ascii_lowercase().as_str() {
            "none" => Some(ReasoningEffort::None),
            "low" => Some(ReasoningEffort::Low),
            "medium" => Some(ReasoningEffort::Medium),
            "high" => Some(ReasoningEffort::High),
            _ => None,
        }
    }

    /// The effective base URL: `base_url` config wins, then `OVEN_BASE_URL`.
    pub fn effective_base_url(&self) -> Option<String> {
        if let Some(u) = &self.base_url {
            return Some(u.clone());
        }
        env::var("OVEN_BASE_URL").ok().filter(|v| !v.is_empty())
    }

    /// The effective API key: `api_key` config wins, then `OVEN_API_KEY`.
    pub fn effective_api_key(&self) -> String {
        if let Some(k) = &self.api_key {
            return k.clone();
        }
        env::var("OVEN_API_KEY").unwrap_or_default()
    }

    /// True when no API key is configured, so interactive sessions should
    /// open `/setup` instead of failing at startup.
    pub fn needs_setup(&self) -> bool {
        self.effective_api_key().is_empty()
    }

    /// Each declared model with [`metadata`](Self::metadata) filled in, keyed
    /// by wire id. The stored entries stay as written, so saving never bakes an
    /// inherited value into a model that only meant to follow the provider.
    pub fn effective_models(&self) -> impl Iterator<Item = (&str, ModelMetadata)> + '_ {
        self.models.iter().map(|(id, metadata)| {
            let mut params = metadata.clone();
            params.fill_missing(&self.metadata);
            (id.as_str(), params)
        })
    }

    /// Overlay `Some` fields from `overlay` onto `self`.
    pub fn merge_fields(&mut self, overlay: &ProviderConfig) {
        if let Some(n) = &overlay.name {
            self.name = Some(n.into());
        }
        if let Some(m) = &overlay.model {
            self.model = Some(m.into());
        }
        if let Some(u) = &overlay.base_url {
            self.base_url = Some(u.into());
        }
        if let Some(p) = overlay.protocol {
            self.protocol = Some(p);
        }
        if let Some(k) = &overlay.api_key {
            self.api_key = Some(k.into());
        }
        if let Some(e) = overlay.reasoning_effort {
            self.reasoning_effort = Some(e);
        }
        self.metadata.merge_fields(&overlay.metadata);
        for (id, metadata) in &overlay.models {
            self.models
                .entry(id.clone())
                .or_default()
                .merge_fields(metadata);
        }
        self.normalize();
    }

    /// Copy unset fields from `src`, model by model.
    pub fn fill_missing(&mut self, src: &ProviderConfig) {
        if self.name.is_none() {
            self.name.clone_from(&src.name);
        }
        if self.model.is_none() {
            self.model.clone_from(&src.model);
        }
        if self.base_url.is_none() {
            self.base_url.clone_from(&src.base_url);
        }
        if self.protocol.is_none() {
            self.protocol = src.protocol;
        }
        if self.api_key.is_none() {
            self.api_key.clone_from(&src.api_key);
        }
        if self.reasoning_effort.is_none() {
            self.reasoning_effort = src.reasoning_effort;
        }
        self.metadata.fill_missing(&src.metadata);
        for (id, metadata) in &src.models {
            self.models
                .entry(id.clone())
                .or_default()
                .fill_missing(metadata);
        }
        self.normalize();
    }
}

/// `name` trimmed, or `None` when it is unset or blank.
fn non_blank(name: &str) -> Option<&str> {
    let name = name.trim();
    (!name.is_empty()).then_some(name)
}

/// Canonical vendor slug for a configured `name`.
fn canonical_name(name: &str) -> Option<String> {
    non_blank(name).map(canonical_vendor)
}

fn qualify_model(raw: &str, name: Option<&str>) -> String {
    let id = ModelId::from(raw);
    match id.vendor().or_else(|| name.and_then(non_blank)) {
        Some(vendor) => id.qualify(vendor).to_string(),
        None => raw.to_string(),
    }
}

fn wire_model(raw: &str) -> String {
    let id = ModelId::from(raw);
    match id.variant() {
        Some(variant) => format!("{}:{variant}", id.wire_id()),
        None => id.wire_id().to_string(),
    }
}

/// Per-process behavioural knobs that are provider-agnostic.
#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    /// Active provider, written as the root `active` key. The slug is also the
    /// `[providers.<slug>]` table key, so the saved file never repeats it.
    pub active_provider: ProviderSelection,
    /// Saved vendors keyed by canonical slug (`deepseek`, `xai`, …).
    pub providers: BTreeMap<String, ProviderConfig>,
    pub request_timeout_secs: u64,
    pub max_retries: u32,
    pub base_backoff_ms: u64,
    /// Fraction of the model's context window that triggers automatic
    /// history compaction after a turn completes. `0` disables it. Has no
    /// effect when the active model's window size is unknown.
    pub compact_threshold: f64,
    /// Provider round trips one turn may take before the loop asks whether to
    /// keep going. Bounds what a single user request can spend.
    pub max_iters: usize,
    /// Tools to mount, by name (`file_read`, `file_write`, `bash`). Empty
    /// means the built-in default set.
    pub tools: Vec<String>,
    /// Delegation to subagents.
    pub subagents: SubagentConfig,
    /// MCP server declarations. Key is the local id used to refer to a server.
    pub mcps: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderSelection {
    pub name: String,
}

#[derive(Debug, Deserialize)]
struct RawAppConfig {
    #[serde(default)]
    active: Option<String>,
    /// Legacy `[provider]` block, whose `name` used to carry the selection.
    #[serde(default)]
    provider: Option<ProviderConfig>,
    #[serde(default)]
    providers: BTreeMap<String, ProviderConfig>,
    #[serde(default = "default_request_timeout_secs")]
    request_timeout_secs: u64,
    #[serde(default = "default_max_retries")]
    max_retries: u32,
    #[serde(default = "default_base_backoff_ms")]
    base_backoff_ms: u64,
    #[serde(default = "default_compact_threshold")]
    compact_threshold: f64,
    #[serde(default = "default_max_iters")]
    max_iters: usize,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    subagents: SubagentConfig,
    #[serde(default)]
    mcps: BTreeMap<String, McpServerConfig>,
}

/// Model declarations, written as a map keyed by wire id. An entry that sets
/// nothing of its own is dropped when the provider declares metadata to
/// inherit; without such metadata it is what makes the id known.
struct ModelsRef<'a> {
    models: &'a BTreeMap<String, ModelMetadata>,
    inheriting: bool,
}

impl Serialize for ModelsRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        for (id, metadata) in self.models {
            if !self.inheriting || !metadata.is_empty() {
                map.serialize_entry(id, metadata)?;
            }
        }
        map.end()
    }
}

/// The canonical on-disk shape: `active` at the root and providers keyed by
/// slug. `active_provider` is skipped: the `active` key already carries it.
impl Serialize for AppConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        if !self.active_provider.name.is_empty() {
            map.serialize_entry("active", &self.active_provider.name)?;
        }
        map.serialize_entry("request_timeout_secs", &self.request_timeout_secs)?;
        map.serialize_entry("max_retries", &self.max_retries)?;
        map.serialize_entry("base_backoff_ms", &self.base_backoff_ms)?;
        map.serialize_entry("compact_threshold", &self.compact_threshold)?;
        map.serialize_entry("max_iters", &self.max_iters)?;
        if !self.tools.is_empty() {
            map.serialize_entry("tools", &self.tools)?;
        }
        if self.subagents != SubagentConfig::default() {
            map.serialize_entry("subagents", &self.subagents)?;
        }
        if !self.mcps.is_empty() {
            map.serialize_entry("mcps", &self.mcps)?;
        }
        if !self.providers.is_empty() {
            map.serialize_entry("providers", &self.providers)?;
        }
        map.end()
    }
}

/// How subagents are allowed to run. The tools a subagent may use come from
/// its role, not from here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubagentConfig {
    /// Mount the delegation tools. Disabled hides `task` and `task_output`.
    pub enabled: bool,
    /// Subagents allowed to run at once. Further spawns wait for a slot.
    pub max_concurrent: usize,
    /// Provider round trips one subagent may take.
    pub max_iters: usize,
}

impl Default for SubagentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_concurrent: DEFAULT_SUBAGENT_CONCURRENT,
            max_iters: DEFAULT_SUBAGENT_ITERS,
        }
    }
}

impl<'de> Deserialize<'de> for AppConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawAppConfig::deserialize(deserializer)?;
        let mut config = Self {
            active_provider: ProviderSelection::default(),
            providers: BTreeMap::new(),
            request_timeout_secs: raw.request_timeout_secs,
            max_retries: raw.max_retries,
            base_backoff_ms: raw.base_backoff_ms,
            compact_threshold: raw.compact_threshold,
            max_iters: raw.max_iters,
            tools: raw.tools,
            subagents: raw.subagents,
            mcps: raw.mcps,
        };

        for (key, mut provider) in raw.providers {
            provider.normalize();
            let name = provider
                .name
                .clone()
                .unwrap_or_else(|| canonical_vendor(&key));
            provider.name = Some(name.clone());
            config
                .providers
                .entry(name)
                .or_default()
                .merge_fields(&provider);
        }

        // Legacy `[provider]` block: its `name` was the selection and its
        // other fields an override on top of the matching saved vendor.
        let legacy = raw.provider.map(|mut provider| {
            provider.normalize();
            let name = provider.name.clone().unwrap_or_default();
            provider.name = Some(name.clone());
            config
                .providers
                .entry(name.clone())
                .or_default()
                .merge_fields(&provider);
            name
        });

        let active = raw
            .active
            .map(|name| canonical_vendor(&name))
            .or(legacy)
            .or_else(|| {
                (config.providers.len() == 1)
                    .then(|| config.providers.keys().next().cloned().unwrap())
            });
        if let Some(active) = active.filter(|name| config.providers.contains_key(name)) {
            config.active_provider.name = active;
        }

        Ok(config)
    }
}

fn default_request_timeout_secs() -> u64 {
    60
}
fn default_max_retries() -> u32 {
    2
}
fn default_base_backoff_ms() -> u64 {
    500
}
fn default_compact_threshold() -> f64 {
    0.8
}
fn default_max_iters() -> usize {
    DEFAULT_MAX_ITERS
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            active_provider: ProviderSelection {
                name: "deepseek".into(),
            },
            providers: BTreeMap::from([(
                "deepseek".into(),
                ProviderConfig {
                    name: Some("deepseek".into()),
                    ..Default::default()
                },
            )]),
            request_timeout_secs: default_request_timeout_secs(),
            max_retries: default_max_retries(),
            base_backoff_ms: default_base_backoff_ms(),
            compact_threshold: default_compact_threshold(),
            max_iters: default_max_iters(),
            tools: Vec::new(),
            subagents: SubagentConfig::default(),
            mcps: BTreeMap::new(),
        }
    }
}

impl AppConfig {
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }

    pub fn base_backoff(&self) -> Duration {
        Duration::from_millis(self.base_backoff_ms)
    }

    /// Apply `overlay` on top of `self`. Non-default fields in `overlay` win.
    pub fn merge(&mut self, overlay: AppConfig) {
        if !overlay.active_provider.name.is_empty() {
            self.active_provider = overlay.active_provider;
        }
        for (name, mut provider) in overlay.providers {
            provider.normalize();
            self.providers
                .entry(canonical_vendor(&name))
                .or_default()
                .merge_fields(&provider);
        }
        if overlay.request_timeout_secs != default_request_timeout_secs() {
            self.request_timeout_secs = overlay.request_timeout_secs;
        }
        if overlay.max_retries != default_max_retries() {
            self.max_retries = overlay.max_retries;
        }
        if overlay.base_backoff_ms != default_base_backoff_ms() {
            self.base_backoff_ms = overlay.base_backoff_ms;
        }
        #[allow(
            clippy::float_cmp,
            reason = "overlay values only override when non-default"
        )]
        if overlay.compact_threshold != default_compact_threshold() {
            self.compact_threshold = overlay.compact_threshold;
        }
        for name in overlay.tools {
            if !self.tools.contains(&name) {
                self.tools.push(name);
            }
        }
        self.mcps.extend(overlay.mcps);
    }

    pub fn active_provider_config(&self) -> Option<&ProviderConfig> {
        self.providers.get(&self.active_provider.name)
    }

    pub fn active_provider_config_mut(&mut self) -> Option<&mut ProviderConfig> {
        self.providers.get_mut(&self.active_provider.name)
    }

    /// True when neither the active provider nor any saved vendor has a key.
    pub fn needs_setup(&self) -> bool {
        self.providers.values().all(ProviderConfig::needs_setup)
    }

    /// Canonical slugs that already have a saved (non-empty) API key.
    pub fn configured_providers(&self) -> Vec<String> {
        self.providers
            .iter()
            .filter(|(_, provider)| !provider.needs_setup())
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn registerable_providers(&self) -> impl Iterator<Item = &ProviderConfig> {
        self.providers
            .values()
            .filter(|provider| !provider.needs_setup())
    }

    pub fn select_provider(&mut self, name: &str) -> Result<(), ConfigError> {
        let name = canonical_vendor(name);
        if !self.providers.contains_key(&name) {
            return Err(ConfigError::InvalidProvider(name));
        }
        self.active_provider.name = name;
        Ok(())
    }

    /// Read one file on its own: no default merged in, so a caller that means
    /// to rewrite the file does not write another file's values into it.
    pub fn load_file(path: &Path) -> Result<Option<AppConfig>, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let cfg: AppConfig =
                    toml::from_str(&text).map_err(|e| ConfigError::Parse(path.to_path_buf(), e))?;
                Ok(Some(cfg))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ConfigError::Read(path.to_path_buf(), e)),
        }
    }

    /// Load configs from (user, project) files and merge them, with the
    /// project file taking precedence. Missing files are silently ignored.
    pub fn load(
        user_config: Option<&Path>,
        project_config: Option<&Path>,
    ) -> Result<Self, ConfigError> {
        let mut cfg = AppConfig::default();
        for path in [user_config, project_config].into_iter().flatten() {
            if let Some(loaded) = Self::load_file(path)? {
                cfg.merge(loaded);
            }
        }
        Ok(cfg)
    }

    /// Default user config location: `~/.oven/config.toml`.
    pub fn default_user_config_path() -> Option<PathBuf> {
        crate::platform::dirs::user_config_path()
    }

    /// Default project config path: `.oven.toml` in the given workspace root.
    pub fn default_project_config_path(root: &Path) -> PathBuf {
        root.join(".oven.toml")
    }

    /// Create a template user config at the default location if it does not
    /// exist yet. Existing configs are left untouched.
    pub fn ensure_user_config() -> Result<(), ConfigError> {
        if let Some(path) = Self::default_user_config_path() {
            Self::ensure_user_config_at(&path)?;
        }
        Ok(())
    }

    fn ensure_user_config_at(path: &Path) -> Result<(), ConfigError> {
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigError::Write(path.to_path_buf(), e))?;
        }
        std::fs::write(path, DEFAULT_USER_CONFIG)
            .map_err(|e| ConfigError::Write(path.to_path_buf(), e))
    }

    /// Update one provider and rewrite the file in the canonical format. The
    /// provider is the overlay's `name`, or the active one when unset.
    pub fn save_provider_at(path: &Path, overlay: &ProviderConfig) -> Result<(), ConfigError> {
        let mut config = Self::load_file(path)?.unwrap_or_default();
        let name = overlay
            .name
            .as_deref()
            .and_then(canonical_name)
            .or_else(|| {
                (!config.active_provider.name.is_empty())
                    .then(|| config.active_provider.name.clone())
            })
            .ok_or_else(|| ConfigError::InvalidProvider("provider name is required".into()))?;
        let provider = config.providers.entry(name.clone()).or_default();
        provider.merge_fields(overlay);
        provider.name = Some(name.clone());
        config.active_provider.name = name;
        Self::save_at(path, &config)
    }

    /// Write a whole config to `path` in the canonical format, creating the
    /// parent directory when it does not exist yet.
    pub fn save_at(path: &Path, config: &AppConfig) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigError::Write(path.to_path_buf(), e))?;
        }
        let text = toml::to_string_pretty(config)
            .map_err(|e| ConfigError::Serialize(path.to_path_buf(), e))?;
        std::fs::write(path, text).map_err(|e| ConfigError::Write(path.to_path_buf(), e))
    }

    /// One provider's `[providers.<slug>]` block exactly as it would be
    /// written, with the API key masked so a preview can be shown and kept.
    pub fn provider_toml(provider: &ProviderConfig) -> Result<String, ConfigError> {
        let mut provider = provider.clone();
        if provider.api_key.is_some() {
            provider.api_key = Some(REDACTED_KEY.to_string());
        }
        let slug = provider.name.clone().unwrap_or_default();
        let providers = BTreeMap::from([(slug, provider)]);
        toml::to_string_pretty(&BTreeMap::from([("providers", providers)]))
            .map_err(|e| ConfigError::Serialize(PathBuf::from("<preview>"), e))
    }

    /// No saved providers and no selection: the starting point for a config
    /// file that does not exist yet. [`Default`] instead seeds the builtin
    /// `deepseek` entry, which must not be baked into a new file.
    pub fn empty() -> Self {
        Self {
            providers: BTreeMap::new(),
            active_provider: ProviderSelection::default(),
            ..Self::default()
        }
    }

    /// Drop a saved provider. When it was the active one, the first remaining
    /// provider takes over; with none left the selection is cleared and the
    /// next interactive start opens `/setup`.
    pub fn remove_provider(&mut self, name: &str) -> bool {
        let name = canonical_vendor(name);
        if self.providers.remove(&name).is_none() {
            return false;
        }
        if self.active_provider.name == name {
            self.active_provider.name = self.providers.keys().next().cloned().unwrap_or_default();
        }
        true
    }

    /// Drop one declared model. `Some(true)` also means `model` named it and
    /// the provider now falls back to its preset (`OVEN_MODEL`, then the
    /// vendor default) rather than to a model that no longer exists; `None`
    /// means nothing matched, so there was nothing to remove.
    pub fn remove_model(&mut self, name: &str, id: &str) -> Option<bool> {
        let wire_id = wire_model(id);
        let provider = self.providers.get_mut(&canonical_vendor(name))?;
        let declared = provider.models.remove(&wire_id).is_some();
        let selected = provider.model.as_deref() == Some(wire_id.as_str());
        if selected {
            provider.model = None;
        }
        (declared || selected).then_some(selected)
    }
}

/// Template written to the user config location on first run. Sourced from
/// `config.example.toml` so the example and the default template stay in sync.
const DEFAULT_USER_CONFIG: &str = include_str!("../../config.example.toml");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_user_config_creates_template_once() {
        let tmp = tempdir::TempDir::new("oven-config").unwrap();
        let path = tmp.path().join("config.toml");
        AppConfig::ensure_user_config_at(&path).unwrap();
        assert!(path.exists());
        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        let mut expected = AppConfig::default();
        expected.merge(toml::from_str(DEFAULT_USER_CONFIG).unwrap());
        assert_eq!(cfg, expected);

        std::fs::write(
            &path,
            "active = \"deepseek\"\n[providers.deepseek]\nmodel = \"edited\"\n",
        )
        .unwrap();
        AppConfig::ensure_user_config_at(&path).unwrap();
        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().model.as_deref(),
            Some("edited")
        );
    }

    #[test]
    fn model_metadata_parses_merges_and_roundtrips() {
        let cfg: AppConfig = toml::from_str(
            "active = \"myproxy\"\n\n[providers.myproxy]\napi_key = \"k\"\n\n[providers.myproxy.models.\"my-model\"]\ncontext_window = 200000\nmax_output_tokens = 8192\nsupports_vision = false\n",
        )
        .unwrap();
        let provider = cfg.active_provider_config().unwrap();
        let params = &provider.models["my-model"];
        assert_eq!(params.context_window, Some(200_000));
        assert_eq!(params.max_output_tokens, Some(8192));
        assert_eq!(params.supports_vision, Some(false));
        assert_eq!(params.supports_tools, None);

        let mut base = ProviderConfig::default();
        base.merge_fields(provider);
        assert_eq!(base.models["my-model"].context_window, Some(200_000));

        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(text.contains("[providers.myproxy.models.my-model]"));
        assert!(!text.contains("id ="));
        let reparsed: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(reparsed, cfg);
    }

    #[test]
    fn legacy_model_entries_still_parse() {
        let cfg: AppConfig = toml::from_str(
            "active = \"myproxy\"\n\n[[providers.myproxy.models]]\nid = \"my-model\"\ncontext_window = 200000\n",
        )
        .unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().models["my-model"].context_window,
            Some(200_000)
        );
    }

    #[test]
    fn legacy_provider_block_selects_and_overrides() {
        let cfg: AppConfig = toml::from_str(
            "[provider]\nname = \"grok\"\nreasoning_effort = \"high\"\n\n[providers.xai]\napi_key = \"xai-key\"\nmodel = \"grok-4.6\"\n",
        )
        .unwrap();
        assert_eq!(cfg.active_provider.name, "xai");
        let provider = cfg.active_provider_config().unwrap();
        assert_eq!(provider.name.as_deref(), Some("xai"));
        assert_eq!(provider.api_key.as_deref(), Some("xai-key"));
        assert_eq!(provider.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(provider.model.as_deref(), Some("grok-4.6"));
    }

    #[test]
    fn provider_metadata_fills_every_model_that_leaves_a_field_unset() {
        let cfg: AppConfig = toml::from_str(
            r#"
active = "myproxy"

[providers.myproxy]
api_key = "k"
context_window = 200000
supports_vision = false

[providers.myproxy.models."a"]
max_output_tokens = 4096

[providers.myproxy.models."b"]
max_output_tokens = 8192
supports_vision = true
"#,
        )
        .unwrap();
        let provider = cfg.active_provider_config().unwrap();
        let models: Vec<_> = provider.effective_models().collect();
        assert_eq!(models[0].0, "a");
        assert_eq!(models[0].1.context_window, Some(200_000));
        assert_eq!(models[0].1.supports_vision, Some(false));
        assert_eq!(models[0].1.max_output_tokens, Some(4096));
        assert_eq!(models[1].0, "b");
        assert_eq!(models[1].1.context_window, Some(200_000));
        assert_eq!(models[1].1.max_output_tokens, Some(8192));
        assert_eq!(models[1].1.supports_vision, Some(true));
        // The stored entries stay as written, so a save never bakes the
        // inherited values in.
        assert_eq!(provider.models["a"].context_window, None);
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(text.contains("context_window = 200000"), "{text}");
        assert!(text.contains("[providers.myproxy.models.a]"), "{text}");
        assert_eq!(toml::from_str::<AppConfig>(&text).unwrap(), cfg);
    }

    #[test]
    fn a_model_that_only_inherits_is_dropped_on_save() {
        let cfg: AppConfig = toml::from_str(
            "active = \"p\"\n\n[providers.p]\ncontext_window = 200000\n\n[providers.p.models.\"m\"]\n",
        )
        .unwrap();
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(!text.contains("models."), "{text}");
        let reloaded: AppConfig = toml::from_str(&text).unwrap();
        assert!(reloaded.providers["p"].models.is_empty());
        assert_eq!(
            reloaded.providers["p"].metadata.context_window,
            Some(200_000)
        );
    }

    #[test]
    fn an_id_only_model_survives_when_no_metadata_is_declared() {
        let cfg: AppConfig =
            toml::from_str("active = \"p\"\n\n[providers.p.models.\"m\"]\n").unwrap();
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(text.contains("[providers.p.models.m]"), "{text}");
        let reloaded: AppConfig = toml::from_str(&text).unwrap();
        assert!(reloaded.providers["p"].models.contains_key("m"));
    }

    #[test]
    fn provider_metadata_merges_across_files_field_by_field() {
        let base: AppConfig = toml::from_str(
            "active = \"p\"\n\n[providers.p]\ncontext_window = 200000\nmax_output_tokens = 8192\n",
        )
        .unwrap();
        let overlay: AppConfig =
            toml::from_str("[providers.p]\ncontext_window = 100000\n").unwrap();
        let mut merged = AppConfig::default();
        merged.merge(base);
        merged.merge(overlay);
        let metadata = &merged.providers["p"].metadata;
        assert_eq!(metadata.context_window, Some(100_000));
        assert_eq!(metadata.max_output_tokens, Some(8192));
    }

    #[test]
    fn model_entry_overrides_provider_metadata() {
        let cfg: AppConfig = toml::from_str(
            "active = \"p\"\n\n[providers.p]\ncontext_window = 200000\nmax_output_tokens = 8192\n\n[providers.p.models.\"m\"]\ncontext_window = 1000000\n",
        )
        .unwrap();
        let models: Vec<_> = cfg
            .active_provider_config()
            .unwrap()
            .effective_models()
            .collect();
        assert_eq!(models[0].1.context_window, Some(1_000_000));
        assert_eq!(models[0].1.max_output_tokens, Some(8192));
    }

    #[test]
    fn model_entries_merge_by_id_and_stay_sorted() {
        let base: AppConfig = toml::from_str(
            "[provider]\nname = \"p\"\n\n[[providers.p.models]]\nid = \"b\"\ncontext_window = 1\n",
        )
        .unwrap();
        let overlay: AppConfig =
            toml::from_str("[[providers.p.models]]\nid = \"a\"\ncontext_window = 2\n\n[[providers.p.models]]\nid = \"b\"\ncontext_window = 3\n")
                .unwrap();
        let mut merged = AppConfig::default();
        merged.merge(base);
        merged.merge(overlay);
        let models: Vec<_> = merged.providers["p"]
            .models
            .iter()
            .map(|(id, metadata)| (id.as_str(), metadata.context_window))
            .collect();
        assert_eq!(models, vec![("a", Some(2)), ("b", Some(3))]);
    }

    #[test]
    fn compact_threshold_defaults_and_merges() {
        let cfg: AppConfig = toml::from_str("[provider]\nname = \"deepseek\"\n").unwrap();
        assert_eq!(cfg.compact_threshold, 0.8);

        let mut base = AppConfig::default();
        let overlay: AppConfig =
            toml::from_str("compact_threshold = 0.5\n[provider]\nname = \"deepseek\"\n").unwrap();
        base.merge(overlay);
        assert_eq!(base.compact_threshold, 0.5);
    }

    #[test]
    fn old_kind_field_is_ignored() {
        let cfg: AppConfig =
            toml::from_str("[provider]\nname = \"deepseek\"\nkind = \"responses\"\n").unwrap();
        assert!(cfg.active_provider_config().unwrap().protocol.is_none());

        let cfg: AppConfig =
            toml::from_str("[provider]\nname = \"deepseek\"\nkind = \"chat\"\n").unwrap();
        assert!(cfg.active_provider_config().unwrap().protocol.is_none());
    }

    #[test]
    fn old_grok_config_canonicalizes_name_and_qualifies_model() {
        let tmp = tempdir::TempDir::new("oven-old-grok").unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "[provider]\nname = \"grok\"\nmodel = \"xai/grok-4.6\"\nkind = \"responses\"\n",
        )
        .unwrap();
        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().name.as_deref(),
            Some("xai")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().model.as_deref(),
            Some("grok-4.6")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().effective_model(),
            "xai/grok-4.6"
        );
        assert!(cfg.active_provider_config().unwrap().protocol.is_none());
    }

    #[test]
    fn effective_provider_name_uses_canonical_name() {
        let cfg = ProviderConfig {
            name: Some("grok".into()),
            ..Default::default()
        };
        assert_eq!(cfg.effective_provider_name(), ProviderName::Grok);
        assert_eq!(
            ProviderConfig::default().effective_provider_name(),
            ProviderName::DeepSeek
        );
        assert_eq!(
            ProviderConfig {
                model: Some("plain-id".into()),
                ..Default::default()
            }
            .effective_provider_name(),
            ProviderName::Custom("unknown".into())
        );
        assert_eq!(ProviderName::from("kimi"), ProviderName::Moonshot);
        assert_eq!(
            ProviderName::from("my-gateway"),
            ProviderName::Custom("my-gateway".into())
        );
    }

    #[test]
    fn effective_model_falls_back_to_default() {
        let cfg = ProviderConfig::default();
        assert_eq!(cfg.effective_model(), "deepseek/deepseek-v4-flash");

        let cfg = ProviderConfig {
            model: Some("deepseek-v4-flash".into()),
            name: Some("deepseek".into()),
            ..Default::default()
        };
        assert_eq!(cfg.effective_model(), "deepseek/deepseek-v4-flash");

        // A vendor with no preset falls back under its own name, not deepseek's.
        let cfg = ProviderConfig {
            name: Some("my-proxy".into()),
            ..Default::default()
        };
        assert_eq!(cfg.effective_model(), "my-proxy/deepseek-v4-flash");
    }

    #[test]
    fn configured_credentials_win_over_env() {
        let cfg = ProviderConfig {
            base_url: Some("https://proxy.example".into()),
            api_key: Some("sk-configured".into()),
            ..Default::default()
        };
        assert_eq!(
            cfg.effective_base_url().as_deref(),
            Some("https://proxy.example")
        );
        assert_eq!(cfg.effective_api_key(), "sk-configured");
        assert!(!cfg.needs_setup());
    }

    #[test]
    fn name_presets_base_url_and_model() {
        let mut cfg = ProviderConfig {
            name: Some("moonshot".into()),
            ..Default::default()
        };
        cfg.apply_name_presets();
        assert_eq!(cfg.base_url.as_deref(), Some("https://api.moonshot.cn/v1"));
        assert_eq!(cfg.model.as_deref(), Some("kimi-k3"));
        assert_eq!(cfg.effective_model(), "moonshot/kimi-k3");
        assert_eq!(ProviderConfig::suggested_model("grok"), Some("grok-4.6"));
        assert_eq!(
            ProviderConfig {
                name: Some("zhipu".into()),
                ..Default::default()
            }
            .effective_model(),
            "zhipu/glm-5.3"
        );
        let mut grok = ProviderConfig {
            name: Some("grok".into()),
            ..Default::default()
        };
        grok.apply_name_presets();
        assert_eq!(grok.name.as_deref(), Some("xai"));
        assert_eq!(grok.model.as_deref(), Some("grok-4.6"));
        assert_eq!(grok.effective_model(), "xai/grok-4.6");
    }

    #[test]
    fn save_provider_at_merges_only_set_fields() {
        let tmp = tempdir::TempDir::new("oven-save-provider").unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "max_retries = 9\n\n[provider]\nname = \"proxy\"\nmodel = \"old\"\n",
        )
        .unwrap();

        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("proxy".into()),
                protocol: Some(ProviderKind::Responses),
                base_url: Some("https://proxy.example".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(cfg.max_retries, 9);
        assert_eq!(
            cfg.active_provider_config().unwrap().model.as_deref(),
            Some("old")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().protocol,
            Some(ProviderKind::Responses)
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().base_url.as_deref(),
            Some("https://proxy.example")
        );
        assert!(cfg.active_provider_config().unwrap().api_key.is_none());
        let text = std::fs::read_to_string(&path).unwrap();
        let retries_at = text.find("max_retries").expect("root max_retries");
        let table_at = text.find("[providers.proxy]").expect("[providers.proxy]");
        assert!(
            text.starts_with("active = \"proxy\"") && retries_at < table_at,
            "root keys must stay above [providers.proxy]: {text}"
        );
    }

    #[test]
    fn save_provider_at_rewrites_legacy_provider_block() {
        let tmp = tempdir::TempDir::new("oven-save-provider-repair").unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "[provider]\nname = \"deepseek\"\nmodel = \"old\"\n").unwrap();

        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("moonshot".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(cfg.max_retries, 2);
        assert_eq!(cfg.request_timeout_secs, 60);
        assert_eq!(cfg.active_provider_config().unwrap().model.as_deref(), None);
        assert_eq!(
            cfg.active_provider_config().unwrap().effective_model(),
            "moonshot/kimi-k3"
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().name.as_deref(),
            Some("moonshot")
        );
        assert_eq!(cfg.providers["deepseek"].model.as_deref(), Some("old"));
        assert!(text.contains("active = \"moonshot\""));
        assert!(!text.contains("[provider]\n"));
        assert!(text.find("max_retries").unwrap() < text.find("[providers.").unwrap());
    }

    #[test]
    fn save_provider_at_writes_reasoning_effort() {
        let tmp = tempdir::TempDir::new("oven-save-effort").unwrap();
        let path = tmp.path().join("config.toml");
        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("openai".into()),
                model: Some("gpt-4o".into()),
                reasoning_effort: Some(ReasoningEffort::Medium),
                ..Default::default()
            },
        )
        .unwrap();

        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().model.as_deref(),
            Some("gpt-4o")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().reasoning_effort,
            Some(ReasoningEffort::Medium)
        );
    }

    #[test]
    fn load_migrates_legacy_provider_into_map() {
        let tmp = tempdir::TempDir::new("oven-hydrate-legacy").unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "[provider]\nname = \"deepseek\"\napi_key = \"sk-old\"\nmodel = \"deepseek-v4-flash\"\n",
        )
        .unwrap();
        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().name.as_deref(),
            Some("deepseek")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().api_key.as_deref(),
            Some("sk-old")
        );
        let saved = cfg.providers.get("deepseek").expect("hydrated");
        assert_eq!(saved.api_key.as_deref(), Some("sk-old"));
        assert_eq!(saved.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(cfg.configured_providers(), vec!["deepseek"]);
        assert!(!cfg.needs_setup());
    }

    #[test]
    fn save_second_vendor_keeps_first() {
        let tmp = tempdir::TempDir::new("oven-save-two").unwrap();
        let path = tmp.path().join("config.toml");
        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("deepseek".into()),
                api_key: Some("sk-ds".into()),
                model: Some("deepseek-v4-flash".into()),
                ..Default::default()
            },
        )
        .unwrap();
        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("xai".into()),
                api_key: Some("xai-key".into()),
                model: Some("grok-4.6".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().name.as_deref(),
            Some("xai")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().model.as_deref(),
            Some("grok-4.6")
        );
        assert_eq!(cfg.providers["deepseek"].api_key.as_deref(), Some("sk-ds"));
        assert_eq!(cfg.providers["xai"].api_key.as_deref(), Some("xai-key"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[providers.deepseek]"));
        assert!(text.contains("[providers.xai]"));
    }

    #[test]
    fn save_model_updates_active_and_saved_model_not_api_key() {
        let tmp = tempdir::TempDir::new("oven-save-model").unwrap();
        let path = tmp.path().join("config.toml");
        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("deepseek".into()),
                api_key: Some("sk-ds".into()),
                model: Some("deepseek-v4-flash".into()),
                ..Default::default()
            },
        )
        .unwrap();
        AppConfig::save_provider_at(
            &path,
            &ProviderConfig {
                name: Some("deepseek".into()),
                model: Some("deepseek-chat".into()),
                reasoning_effort: Some(ReasoningEffort::High),
                ..Default::default()
            },
        )
        .unwrap();

        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().model.as_deref(),
            Some("deepseek-chat")
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().reasoning_effort,
            Some(ReasoningEffort::High)
        );
        assert_eq!(
            cfg.active_provider_config().unwrap().api_key.as_deref(),
            Some("sk-ds")
        );
        assert_eq!(cfg.providers["deepseek"].api_key.as_deref(), Some("sk-ds"));
        assert_eq!(
            cfg.providers["deepseek"].model.as_deref(),
            Some("deepseek-chat")
        );
    }

    #[test]
    fn load_selects_active_provider_from_map() {
        let tmp = tempdir::TempDir::new("oven-hydrate-map").unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "[provider]\nname = \"xai\"\nmodel = \"grok-4.6\"\n\n[providers.xai]\napi_key = \"xai-key\"\n[providers.deepseek]\napi_key = \"sk-ds\"\nmodel = \"deepseek-v4-flash\"\n",
        )
        .unwrap();
        let cfg = AppConfig::load(None, Some(&path)).unwrap();
        assert_eq!(
            cfg.active_provider_config().unwrap().api_key.as_deref(),
            Some("xai-key")
        );
        assert_eq!(cfg.providers["deepseek"].api_key.as_deref(), Some("sk-ds"));
        assert_eq!(cfg.configured_providers(), vec!["deepseek", "xai"]);
    }

    #[test]
    fn save_at_writes_only_what_the_config_holds() {
        let tmp = tempdir::TempDir::new("oven-save-at").unwrap();
        let path = tmp.path().join("nested").join("config.toml");
        let mut config = AppConfig::empty();
        config.providers.insert(
            "myproxy".into(),
            ProviderConfig {
                name: Some("myproxy".into()),
                api_key: Some("sk-secret".into()),
                model: Some("my-model".into()),
                ..Default::default()
            },
        );
        config.active_provider.name = "myproxy".into();
        AppConfig::save_at(&path, &config).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("active = \"myproxy\""), "{text}");
        assert!(text.contains("[providers.myproxy]"), "{text}");
        assert_eq!(AppConfig::load_file(&path).unwrap().unwrap(), config);
    }

    #[test]
    fn empty_config_writes_no_provider_and_no_selection() {
        let text = toml::to_string_pretty(&AppConfig::empty()).unwrap();
        assert!(!text.contains("active"), "{text}");
        assert!(!text.contains("deepseek"), "{text}");
        assert!(!text.contains("providers"), "{text}");
    }

    #[test]
    fn remove_provider_reselects_the_first_remaining() {
        let mut config: AppConfig = toml::from_str(
            "active = \"xai\"\n\n[providers.xai]\napi_key = \"x\"\n[providers.deepseek]\napi_key = \"d\"\n",
        )
        .unwrap();
        assert!(config.remove_provider("xai"));
        assert_eq!(config.active_provider.name, "deepseek");
        assert!(!config.providers.contains_key("xai"));
        assert!(!config.remove_provider("xai"));
    }

    #[test]
    fn remove_provider_clears_the_selection_when_it_was_the_last() {
        let mut config: AppConfig =
            toml::from_str("active = \"xai\"\n\n[providers.xai]\napi_key = \"x\"\n").unwrap();
        assert!(config.remove_provider("grok"));
        assert!(config.providers.is_empty());
        assert!(config.active_provider.name.is_empty());
        let text = toml::to_string_pretty(&config).unwrap();
        assert!(!text.contains("active"), "{text}");
    }

    #[test]
    fn remove_model_clears_the_selected_model() {
        let mut config: AppConfig = toml::from_str(
            "active = \"p\"\n\n[providers.p]\nmodel = \"gpt-4o\"\n\n[providers.p.models.\"gpt-4o\"]\ncontext_window = 128000\n",
        )
        .unwrap();
        assert!(config.remove_model("p", "gpt-4o").is_some());
        let provider = &config.providers["p"];
        assert!(provider.model.is_none());
        assert!(provider.models.is_empty());
    }

    #[test]
    fn remove_model_keeps_an_unrelated_selection() {
        let mut config: AppConfig = toml::from_str(
            "active = \"p\"\n\n[providers.p]\nmodel = \"kept\"\n\n[providers.p.models.\"dropped\"]\n\n[providers.p.models.\"kept\"]\n",
        )
        .unwrap();
        assert_eq!(config.remove_model("p", "dropped"), Some(false));
        let provider = &config.providers["p"];
        assert_eq!(provider.model.as_deref(), Some("kept"));
        assert!(!provider.models.contains_key("dropped"));
        assert_eq!(config.remove_model("p", "dropped"), None);
        assert_eq!(config.remove_model("nope", "kept"), None);
    }

    #[test]
    fn remove_model_reports_clearing_the_selection() {
        let mut config: AppConfig =
            toml::from_str("active = \"p\"\n\n[providers.p]\nmodel = \"gpt-4o\"\n").unwrap();
        assert_eq!(config.remove_model("p", "gpt-4o"), Some(true));
        assert!(config.providers["p"].model.is_none());
    }

    #[test]
    fn provider_toml_masks_the_api_key() {
        let provider = ProviderConfig {
            name: Some("myproxy".into()),
            base_url: Some("https://proxy.example/v1".into()),
            api_key: Some("sk-secret".into()),
            model: Some("my-model".into()),
            ..Default::default()
        };
        let text = AppConfig::provider_toml(&provider).unwrap();
        assert!(text.contains("[providers.myproxy]"), "{text}");
        assert!(text.contains("<redacted>"), "{text}");
        assert!(!text.contains("sk-secret"), "{text}");
    }
}

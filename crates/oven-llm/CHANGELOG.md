# Changelog

## [0.5.0] - 2026-10-05

### Changed
- oven-llm now lives in the [oven](https://github.com/guuzaa/oven) repository at
  `crates/oven-llm` and is published from there. It is no longer a separate
  external crate. `repository` is `https://github.com/guuzaa/oven` and
  `homepage` is `https://github.com/guuzaa/oven/tree/master/crates/oven-llm`.
  The published crate still depends only on its own libraries, not on the oven
  application crates.
- Minimum supported Rust version is **1.92** (was 1.88). `edition` and
  `rust-version` follow the oven workspace.
- `Router` is only the vendor table: which provider owns a model. It does not
  implement `Provider`, and it has no `complete`, `stream`, retry, or timeout.
  Hold a `Client` and call the `Provider` trait.
- `upsert` replaces the entry with the same vendor slug and protocol.
  Completions and Responses for one vendor are two registrations.
- `RequestBuilder::thinking(ThinkingMode::Enabled)` does not set a reasoning
  effort. Pass `reasoning_effort` when a level is required.
- Completions and Responses decode errors include the wire reason in
  `Display`.
- A vendor with no preset protocol (Anthropic) builds as Completions when
  `base_url` is set. `Messages` is still unsupported.

### Added
- `Client` is the type callers invoke. `Client::from(router)` implements
  `Provider`. `complete` and `stream` are those trait methods. `Client` derefs
  to `Router`, so `qualify`, `provider`, `resolve_model`, and `known_models`
  stay on the same snapshot.
- `RouterHandle` shares one `Arc<Client>`. `load` stays stable across
  `.await`. `replace` swaps the whole table, so an in-flight call keeps the
  previous providers and the next load sees the new one.
- `RetryingProvider` applies timeout and retries when a provider is built.
  `new` does neither. Opt in with `with_timeout`, `with_retries`, and
  `with_base_backoff`. Transport errors, rate limits, and HTTP 408/429/5xx are
  retried. A `complete` timeout is reported as 408. `stream` retries the
  connection start only and is not timed out. `list_models` is not retried.
  The wait is a `futures-timer` future, so the library stays runtime-agnostic.
- `ProviderName::default_model()` and `ProviderName::known_models()` read the
  static catalog with no client and no API key.
- `ModelInfo::declared` and `ModelCapabilities::supported` /
  `with_overrides`. A declared model treats unset capability flags as
  supported.
- `register` takes `impl Provider + 'static`. `Box<P>` and `Box<dyn Provider>`
  both implement `Provider`.

### Removed
- `RouterError::Provider`. Dispatch failures are `NoProviderRegistered` and
  `UnknownModel`. A provider failure is a `ProviderError` returned by
  `Client`.

## [0.4.2] - 2026-09-19
### Added
- `ContentBlock::ToolUse.raw_arguments`: the `arguments` JSON text exactly as it
  arrived on the wire. Both decoders (streaming via `StreamCollector`, and
  non-streaming) fill it in, and both encoders replay it verbatim.
  `ContentBlock::tool_use(...)` builds a block without it, and
  `ContentBlock::tool_arguments(input, raw)` returns the text to replay
  (raw when present, otherwise the compact `input` serialization).

### Changed
- `ContentBlock::ToolUse` gained a field. Existing struct literals need
  `raw_arguments: None` (or `ContentBlock::tool_use(...)`) and destructuring
  patterns need `..`.

### Fixed
- Replaying an assistant turn no longer re-serializes `tool_calls[].arguments`
  from the parsed `Value` (compact separators, keys sorted alphabetically).
  Providers that cache on disk — DeepSeek in particular — only serve a prefix
  that *fully* matches a stored cache unit, and one of those units ends at the
  model output; a re-serialized tool call (`{"a": "b"}` → `{"a":"b"}`, keys
  reordered) diverges from the generated tokens there, so the whole assistant
  turn — reasoning included — fell out of the cache on every later request.
  Measured against `deepseek-flash`: byte-faithful replay hit 384 tokens (6
  blocks) deeper on the immediately following request, and the hit rate rose
  from 65.5% to 78.6% there.

## [0.4.1] - 2026-08-20
### Added
- `Router::upsert`: replace an existing registration with the same vendor slug,
  or append. Same-vendor multi-protocol still uses `register` (append).

### Changed
- `ProviderBuilder::provider()`: omit `kind` and the builder uses that vendor's
  default protocol. `completions()` / `responses()` still force a single protocol.

## [0.4.0] - 2026-08-19
### Added
- `ModelId` slug parsing: `vendor()` / `wire_id()` / `variant()` / `qualify()`,
  plus `canonical_vendor` (`kimi`→`moonshot`, `glm`→`zhipu`, `grok`→`xai`).
  `Request.model` is now `vendor/wire-id[:variant]` or a bare wire id.
- `Router::qualify` completes a bare id when only one vendor is registered, and
  rewrites vendor aliases (`grok/...` → `xai/...`).
- `Router` implements `Provider`, so an agent can hold one object for a single
  vendor or a mix. `known_models` / `list_models` return slugs and de-duplicate
  across registrations.
- `ProviderBuilder::provider()`: omit `kind` and the builder registers every
  protocol that vendor speaks (DeepSeek → Completions + Responses) as a
  `Router`. `completions()` / `responses()` still force a single protocol.
- `Provider::protocol`, `ProviderName::slug` / `matches_vendor`, and
  `ModelInfo::{protocols, default_protocol, supports_protocol, slug}` so
  protocol selection lives on the catalog instead of the request.
- `ProviderError::NoProviderRegistered` and `ProviderError::UnknownModel`,
  with `From<RouterError>` so `Router` can implement `Provider`.

### Changed
- Router dispatch is slug vendor first (`deepseek/...` → the DeepSeek
  registration), then each provider's static catalog (first registration
  wins). A `:responses` / `:messages` suffix, or the catalog default, picks
  the protocol. No match → `RouterError::UnknownModel`; an explicit vendor
  that is registered is forwarded even if the wire id is not in the catalog.
- Completions and Responses encoders send `ModelId::wire_id()` on the wire,
  not the full slug.
- `ProviderName::Grok` serializes as `"xai"`; `"xai"` and `"grok"` both
  deserialize back to `Grok`.
- Setting `base_url` on `ProviderBuilder` no longer drops the preset catalog;
  extra models append.
- README rewritten around `ProviderBuilder` + `Router` + `vendor/wire-id`
  slugs. `router_usage` now also registers a custom `my-proxy` gateway.

### Removed
- `Router::alias` and `Router::route`, along with the `aliases` / `prefixes`
  fields. Dispatch is slug vendor + static catalog only.
- `ModelRegistry`. Model lookup lives on each provider's static catalog
  (`known_models` / `resolve_model`) and `Router`; callers that need a custom
  list should pass `ModelInfo` through `ProviderBuilder`.

## [0.3.1] - 2026-08-16
### Added
- `RequestBuilder::prompt(...)` appends a user text message, so simple
  single-turn requests no longer need `Message::user_text` at the call site.
- `From<&str>` for `ProviderName`: case-insensitive parsing with aliases
  (`kimi` → Moonshot, `glm` → Zhipu); unknown strings become `Custom`.
- `Display` for `ProviderKind` (lowercase slugs: `completions` / `responses` /
  `messages`).

### Fixed
- Completions encoder now forwards assistant `Thinking` blocks as
  `reasoning_content` on the wire (concatenating interleaved thinking chunks),
  so multi-turn conversations keep the thinking prefix consistent with the
  previous turn instead of dropping it.

## [0.3.0] - 2026-08-14
### Added
- New `Router` routing layer: register multiple providers and dispatch
  `complete` / `stream` by `Request.model`, so callers maintain a single
  "model → provider" registration instead of switching providers manually.
  - Dispatch priority: exact `alias(...)` bindings, then the longest matching
    `route(...)` prefix (earliest rule wins ties), then each provider's static
    model catalog in registration order; no match returns
    `RouterError::UnknownModel` instead of silently routing to the wrong vendor.
  - `RouterError` with `NoProviderRegistered`, `UnknownModel`, and `Provider`
    variants; rules reference providers by `ProviderName` and resolve at
    dispatch time, so `route` / `alias` work before or after `register`.
- New `router_usage` example showing routing across DeepSeek / Zhipu providers.
- `Display` impls for `ProviderName`, `ReasoningEffort`, and `ThinkingMode`
  (wire-style lowercase strings for the latter two).
- `Sub` and `SubAssign` impls for `Usage`, with per-field saturating
  subtraction (complements the existing `Add` / `AddAssign`).

### Fixed
- `RequestBuilder::thinking(ThinkingMode::Enabled)` now defaults
  `reasoning_effort` to `Medium` when it hasn't been set explicitly, so
  enabling thinking no longer requires toggling `reasoning_effort`.
- Completions decoder falls back to `prompt_tokens_details.reasoning_tokens`
  when `completion_tokens_details.reasoning_tokens` is absent, since some
  providers report reasoning tokens under the prompt details.

### Changed
- Completions encoder builds `thinking` / `reasoning_effort` wire values via
  the new `Display` impls (same wire format, less duplicated matching).

## [0.2.2] - 2026-08-07
### Added
- `ProviderName` now uses a hand-written Serialize/Deserialize impl:
  - known providers serialize as lowercase strings ("openai", "deepseek", ...)
  - Custom serializes as "custom(<name>)" with the name normalized to lowercase
  - deserialization lowercases the whole input, so it is case-insensitive
  - unknown strings fall back to Custom for backward compatibility
- `ProviderKind` derives Serialize/Deserialize with lowercase variant names.

## [0.2.1] - 2026-08-03
### Added
- Unified provider creation: `ProviderBuilder` + `ProviderKind` dispatch to
  `CompletionsProvider` / `ResponsesProvider` from one entry point
  (`kind` + `provider_name` + `api_key`, optional `base_url` / `extra_headers` /
  `known_models`), returning `Result<Box<dyn Provider>>`.
- `ProviderError::UnsupportedProvider` and `ProviderError::InvalidProviderConfig`
  for builder failures (the unified path returns errors instead of panicking).
- `ProviderBuilder::add_model` to append a single `ModelInfo` to the builder's
  `known_models` list (complements the bulk `known_models(...)` setter).
- `ProviderBuilder` builds known presets from their preset base URL and static
  model catalog, so `known_models` / `add_model` / `extra_headers` can augment
  presets without requiring a `base_url`.

## [0.2.0] - 2026-08-02
### Added
- OpenAI Responses API support: `ResponsesProvider` with DeepSeek / Grok / OpenAI
  presets, wire types, encoder/decoder, SSE streaming, and a new
  `responses_usage` example.
- Re-export `secrecy::SecretString` as `oven_llm::SecretString` for callers that
  want to wrap API keys explicitly.

### Changed
- Rename `OpenAICompatProvider` to `CompletionsProvider` and the `openai_compat`
  module to `completions` (breaking).
- Remove the aggregate model-list APIs (`all_openai_compat_models`,
  `all_responses_models`) and their re-exports (breaking).
- Sort `ProviderError` variants into a stable, grouped order.
- Extract the shared HTTP transport into `provider/http.rs` (auth headers,
  endpoint joining, status-code mapping, SSE bridging, `list_models`) and use it
  from both providers.
- Provider constructors (`new`, `with_base_url`, `with_models`, and all vendor
  presets) now accept `impl Into<SecretString>`: callers can pass a plain
  `String` or `&str` without depending on `secrecy`; the key is still stored
  internally as `SecretString`.
- Rewrite the README with a feature overview, protocol descriptions, code
  examples, and a contribution guide; rename the `basic_usage` example to
  `completions_usage`.

### Fixed
- Non-streaming `complete` now decodes the first choice instead of rejecting
  responses with multiple `choices`, matching streaming behavior.
- Flaky Responses decoder tests on Windows: SSE test fixtures now handle CRLF
  line endings.

### Removed
- `CompletionsDecodeError::MultipleChoices` and its validation logic.
- Stale design-document references from module-level docs.

## [0.1.3] - 2026-07-30
### Fixed
- Polish Cargo.toml: cut unused files for package
- Replace reqwest with isahc, making this lib asynchronous runtime-agnostic

## [0.1.2] - 2026-07-28
### Added
- New APIs: 
    - OpenAICompatProvider: new, with_base_url
    - Message: system_prompt

### Fixed
- Polish tokio features: rt, macros, rt-multi-thread

## [0.1.1] - 2026-07-25

### Added
- New APIs: 
    - Response: thinking, text, tool_uses, has_tool_use
    - Message: system, user_text, assistant_text, assistant_text, tool_result

### Fixed
- Flaky tests in Provider

### Added

## [0.1.0] - 2026-07-23

### Added
- Initial Release
- Supports OpenAI compatible API
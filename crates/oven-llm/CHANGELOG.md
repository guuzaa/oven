# Changelog

## [Unreleased]

### Added
- `complete_streaming(provider, request, options, on_event)` drives a streamed call and
  returns the assembled `Response`. Every event goes to `on_event` first and is collected
  afterwards, so what the caller saw and what the response holds can no longer drift
  apart — the loop every caller used to write by hand.
- `ToolUse` is a type of its own: `id()`, `name()`, `input()`, `raw_arguments()`,
  `arguments()` and `set_input()`. The wire text the model generated is kept inside it,
  so the cache-hit contract is the type's business and not the caller's.
- `ToolUse::arguments()` reports a truncated argument object as
  `ToolArgumentsError::InvalidJson` instead of handing the caller an empty object. The
  verdict comes from the same parser that read the wire text, so arguments the provider's
  own parser rejects — a number out of range, a nesting too deep — are reported too
  rather than run with empty arguments.
- `StreamEvent::text_delta()`, `thinking_delta()` and `tool_input_delta()` read the
  delta out of an event without matching the nested enum.
- `Response::tool_calls()` hands out typed tool calls, and `Response::stopped_for_tools()`
  says whether the model stopped because it wants tools. It is the strict check: it also
  wants the provider to have reported `tool_calls` as the stop reason. A loop that runs
  whatever came back asks `Response::has_tool_calls()` — that is what the agent does, so
  a call the provider stopped short of is not dropped without the model learning that it
  failed.
- `Message::text()` reads the text out of a message, whatever role it has.

### Changed
- **Breaking:** `ProviderError::RateLimit` carries the reason the provider gave
  (`body`) beside the wait it asked for. What it displays is the reason now:
  `rate limited: quota exceeded`, not `rate limited, retry after Nonems`.
- **Breaking:** `ContentBlock::ToolUse` holds a `ToolUse` instead of four loose fields.
  Its JSON is unchanged — `{"type": "tool_use", "id", "name", "input", "raw_arguments"}` —
  so stored conversations still load. `ContentBlock::tool_use(id, name, input)` builds
  one, and `ContentBlock::as_tool_use()` reads one out of a block.
- **Breaking:** `Response::tool_uses()` is `Response::tool_calls()` and hands out
  `&ToolUse`; `Response::has_tool_use()` is `Response::has_tool_calls()`.
- **Breaking:** `ContentBlock::tool_arguments` and `ContentBlock::parse_tool_arguments`
  are gone. The first is `ToolUse::wire_arguments()`, the second is `ToolUse::from_raw()`.
- **Breaking:** the `Request` and `RequestBuilder` methods say what they do: `message`
  is `push_message`, `messages` is `set_messages`, `tool` is `push_tool`, `tools` is
  `set_tools`. Appending one and replacing the whole collection no longer differ by a
  single letter, and the two types no longer answer to the same name with a different
  meaning. `system`, `prompt` and `tool_choice` keep their names.
- **Breaking:** `Message::system_prompt()` is gone. It read a system prompt out of a
  message, which made a system *message* look like a second place to put the system
  prompt of a request. `Message::text()` reads the text of any message; whether it is
  the system one is the role's business, and the caller's.

### Fixed
- A refused request says why, in the provider's own words. The response body
  was passed on as it came: a JSON envelope around the message, or a proxy's
  HTML error page. The reason is now the message inside `{"error":{"message":
  "..."}}`, or the body collapsed onto one line and cut to 200 characters; a
  body that says nothing falls back to the status (`Too Many Requests`).
- The `Retry-After` header is read. `ProviderError::RateLimit` carries the
  wait the provider asked for, and the retry waits that long.
- An error says why it happened. `ProviderError::Stream`, `InvalidRequest`,
  `CompletionsEncode` and `ResponsesEncode` kept their reason in the source
  chain only, so the text a caller saw was `stream protocol error` or
  `invalid request` with the reason nowhere in it. The reason is part of the
  message now: `stream protocol error: stream ended before [DONE]`.
- A Chat Completions stream that ends before the provider said why is an error, not a
  finished reply. The decoder used to close the open blocks, report a `MessageDelta` with
  no stop reason and hand out a `MessageStop`, so a reply cut off mid-sentence looked
  like the model's answer. It now reports `CompletionsDecodeError::IncompleteStream`
  ("stream ended before the model finished"), the same way the Responses protocol reports
  a stream that ends without `response.completed`.
- A request's system prompt has one place to live: `Request.system`. The protocols carry
  it apart from the messages — the Responses API's `instructions`, the first `system`
  message of a Chat Completions request — so it is not the same thing as a `Role::System`
  message, which is one message of the conversation.
- The `ContentBlock` docs said a `ToolResult` block lives under a `Role::User` message.
  It lives under a `Role::Tool` message (`Message::tool_result` builds one), which is
  what the encoders, the transcript and the turn accounting all read. A `Role::User`
  message may carry one too — the Anthropic shape — and the encoders translate it, but
  that is the alternative, not the rule.

## [0.6.0] - 2026-10-24

### Added
- `ProviderName::MiniMax` and the MiniMax preset: `base_url = https://api.minimax.io/v1`
  (OpenAI-compatible), with `MiniMax-M3` (a thinking switch) and `MiniMax-M3.1-Flash`
  (levels) in the catalog. Every MiniMax request carries `reasoning_split`, so the thinking
  comes back as `reasoning_content` instead of as text.
- `EffortSupport` says what a model can express: `Unknown`, `Unsupported`, `Fixed` (it
  reasons, the depth is not ours to pick), `Toggle` (a switch, no levels) and `Levels`.
  `ModelInfo::effort_support` carries it and `ModelInfo::with_effort_support` sets it.
- `Dialect` says which channel the wire uses: `Level` (`reasoning_effort`), `Toggle`
  (`thinking.type`) or `Both`. `VendorProfile::for_protocol` pairs a protocol with the
  vendor's dialect.
- `ReasoningEffort` grows `Minimal`, `XHigh`, `Max` and `Budget(u32)`. `off` and `zero`
  are aliases of `none`, `x-high` of `xhigh`, and a token budget is written as a number.
  `LADDER` is the strength ladder, and `ParseEffortError` is the error a bad word gives.
- Replay tests over recorded exchanges (`kimi-k3`, `kimi-k2.6`, `minimax-m3`, `grok-4.6`):
  the request body has to come out the same and the response has to decode into the same
  domain objects, so a change in either encoder or decoder shows up as a failing test
  instead of a broken call.

### Changed
- **Breaking:** the request and the options are separate arguments — `Provider::complete`
  and `Provider::stream` take `(&Request, &CallOptions)`. The request is the conversation;
  the options name the model it goes to and how to ask (sampling, reasoning effort,
  private fields). The same conversation can go out to another model or with other
  options without being rebuilt.
- **Breaking:** the model moves from the request to the options — `CallOptions::new(model)`
  is the only way to make one, so a call always names its model, and
  `Request::builder().build()` cannot fail any more: `Request` is the conversation alone.
- **Breaking:** `ReasoningEffort::None` is now `Off` (the wire word stays `none`). An
  effort that is not set means "no preference" and reaches the wire as no field at all;
  before, the Responses protocol folded it into `effort: "none"` and forced the upstream
  default off.
- **Breaking:** `SamplingParams` and `CallOptions` move out of `domain::request` into
  `domain::options`, and `ModelId` into `domain::model`. The crate-root paths are
  unchanged.
- The reasoning effort reaches the wire through the vendor's own dialect: Zhipu gets
  `thinking.type` and no `reasoning_effort`, OpenAI, DeepSeek, Moonshot and xAI get
  `reasoning_effort` and no `thinking`, MiniMax follows the model's own capability, and
  a vendor nobody verified yet — or a proxy that speaks either — keeps sending both. A
  level the model does not have is clamped down to the closest one it has — the set is not
  a ladder (DeepSeek has no `medium`), so the clamp compares strength, not position — and
  the request path never fails on it.
- The sampling parameters are no longer sent by default: the vendor's own default is the
  right one, and the Kimi family fixes its temperature per model (thinking 1.0,
  non-thinking 0.6) and rejects any other value.
- The Moonshot and xAI reasoning parameters are calibrated from the vendor docs: Kimi K3
  takes `reasoning_effort` (`low`/`high`/`max`, no `medium`) and refuses `thinking`,
  `kimi-k2.6` takes the `thinking.type` switch, `kimi-k2.7-code` always thinks and takes
  neither, the Grok models with an effort take `low`/`medium`/`high`/`xhigh`, and the ones
  without one (`grok-build-0.1`) reason at a depth we cannot pick.
- **Breaking:** the HTTP transport is `reqwest` instead of `isahc`, and the crate now needs a
  Tokio runtime. The client is built on Tokio's reactor, so a request issued outside a Tokio
  runtime panics instead of returning an error; backoff and timeouts wait on the Tokio timer. A
  `base_url` that is not a valid `http`/`https` URL is still reported as
  `ProviderError::InvalidProviderConfig` rather than as a transport error.
- **Breaking:** `ProviderError::Transport` carries a `reqwest::Error` instead of an `isahc::Error`.

### Removed
- `ThinkingMode` and `Request::thinking`. The thinking switch is derived from the effort,
  so the two protocols can no longer disagree about whether reasoning is on.
- `BuilderError`. `RequestBuilder::build()` cannot fail any more.
- `ProviderError::Io`. The isahc response body was an `AsyncRead` and its I/O errors were the
  only source for this variant; a `reqwest` response body is a byte stream.
- The `isahc`, `futures-lite` and `futures-timer` dependencies. The crate no longer pulls
  `curl-sys`, `libz-sys` or `libnghttp2-sys`, so it needs no C toolchain and no CMake.

### Fixed
- GLM no longer receives a `reasoning_effort` it does not read: it takes `thinking.type`
  only, so the level used to be dropped in silence while the caller showed it as if it had
  taken effect.
- A model whose reasoning depth is fixed gets no reasoning field at all
  (`EffortSupport::Fixed`), and a model that refuses `thinking: disabled` is not told to
  stop thinking.

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

### Fixed
- Completions streaming: the trailing usage chunk that providers send after
  `finish_reason` (`stream_options.include_usage`) is no longer dropped, so
  `Response.usage` is populated for streamed turns.
- Responses streaming: `response.completed` with `status: "failed"` now fails the
  turn with the wire error message instead of being recorded as a successful
  empty answer, matching the non-streaming path.
- Responses wire types: a known event whose body fails to deserialize is now an
  error instead of falling into the unknown-event variant and being dropped.
- `ResponsesProvider::new` and `CompletionsProvider::new` return
  `ProviderError::UnsupportedProvider` for a `ProviderName` that has no preset
  for that protocol (`Anthropic`, `Moonshot`, `Zhipu`, `Grok`, `Custom`) instead
  of panicking, so a caller building providers from a list can handle each
  failure.
- `post_json` and `list_models` return `ProviderError::InvalidProviderConfig`
  when the configured `base_url` cannot be built into a valid request URL,
  instead of panicking on request construction.
- Exponential backoff is capped at `2^16` times the base delay and uses
  `saturating_mul`, so a large `with_retries` value can no longer overflow
  (a panic in debug builds) or grow without bound.

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
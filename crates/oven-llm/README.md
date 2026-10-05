# oven-llm

[![crates.io](https://img.shields.io/crates/v/oven-llm.svg)](https://crates.io/crates/oven-llm)
[![docs.rs](https://docs.rs/oven-llm/badge.svg)](https://docs.rs/oven-llm)
[![MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A Rust library for calling LLM providers through one unified, async API.

Source lives in the [oven](https://github.com/guuzaa/oven) repository at `crates/oven-llm` and is published on its own. It does not depend on the oven application crates.

Build a vendor with [`ProviderBuilder`](#one-vendor-providerbuilder), register several of them on
a [`Router`](#several-vendors-router), and send `Request.model` as a `vendor/wire-id` slug.
Application code talks to the `Provider` trait and a provider-agnostic domain model
(`Request` / `Response` / `StreamEvent`). Wire formats stay inside the crate.

> [!WARNING]
> **Status: not production-ready.** This crate is under active development. The public API is
> still evolving and may change without notice, and there are no stability or correctness
> guarantees yet. Please evaluate it before using it in production workloads.

## Install

```sh
cargo add oven-llm
```

## One vendor (`ProviderBuilder`)

Do not pick a protocol. `ProviderBuilder::provider()` uses that vendor's default protocol
and returns `Box<dyn Provider>`. Use `completions()` / `responses()` to force one.

```rust
use oven_llm::{Provider, ProviderBuilder, ProviderName, Request};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = ProviderBuilder::provider()
        .provider_name(ProviderName::DeepSeek)
        .api_key(std::env::var("DEEPSEEK_API_KEY")?)
        .build()?;

    let request = Request::builder()
        .model("deepseek/deepseek-v4-flash")
        .prompt("Describe what Rust is")
        .build()?;

    let response = provider.complete(&request).await?;
    println!("{}", response.text());
    Ok(())
}
```

`build()` returns a `Result`. An unsupported vendor (for example Anthropic, which has no
implementation yet) is a typed error, not a panic. Known presets keep their base URL and static
catalog; `.add_model(...)` / `.extra_headers(...)` can extend them. A custom gateway needs
`.base_url(...)` and any `ProviderName`, including `Custom(...)`.

Force a single wire protocol only when you need to:

```rust
let provider = ProviderBuilder::completions() // or ::responses()
    .provider_name(ProviderName::DeepSeek)
    .api_key(api_key)
    .build()?;
```

## Several vendors (`Router`)

Register each vendor once. `Request.model` decides where the call goes — mixing vendors is
configuration, not a `match` in application code.

```rust
use oven_llm::{Client, Provider, ProviderBuilder, ProviderName, Request, Router};

let deepseek = ProviderBuilder::provider()
    .provider_name(ProviderName::DeepSeek)
    .api_key(deepseek_key)
    .build()?;
let zhipu = ProviderBuilder::provider()
    .provider_name(ProviderName::Zhipu)
    .api_key(zhipu_key)
    .build()?;

let mut router = Router::new();
router.register(deepseek).register(zhipu);
let client = Client::from(router);

let request = Request::builder()
    .model("zhipu/glm-5.3")
    .prompt("hello")
    .build()?;

// `Client` implements `Provider`. `complete` and `stream` are the trait methods.
let response = client.complete(&request).await?;
let mut stream = client.stream(&request).await?;
```

`Router` only answers which provider owns a model (`provider`, `qualify`). It does not call
providers and it does not retry. `Client` is that table as a `Provider`.

`register` takes `impl Provider + 'static`, so a concrete client does not need to be boxed first.
`ProviderBuilder::build` still returns `Box<dyn Provider>`, and that box registers as-is.

Retry and timeout are off until you wrap a provider with `RetryingProvider`. When on, transport
errors, rate limits, and HTTP 408/429/5xx are retried. Streaming retries the connection start,
not events already in flight. `list_models` is not retried. The wait is a future, so whichever
executor polls the provider drives the backoff and the timeout.

```rust
let deepseek = oven_llm::RetryingProvider::new(deepseek)
    .with_timeout(std::time::Duration::from_secs(60))
    .with_retries(2);
```

`RouterHandle` shares one `Client` snapshot: `load` clones an `Arc<Client>` that stays stable
across `.await`, and `replace` swaps the whole table. In-flight loads keep the previous providers.

Dispatch:

1. The vendor segment of the slug (`deepseek/...` → the DeepSeek registration).
2. Each provider's static catalog (first registration wins).
3. No match → `RouterError::UnknownModel` from `Router::provider`. `Client`'s `complete` / `stream`
   surface that as `ProviderError::UnknownModel`.

Prefer `vendor/wire-id` (`deepseek/deepseek-v4-flash`). A bare id is qualified when the router
only has one vendor. Protocol comes from the catalog, or from an optional `:responses` suffix —
callers do not pick `ProviderKind` per request.

`Client` implements `Provider`, so an agent holds one object for both a single vendor and a mix.
The trait methods are the call. `Router` stays the vendor table behind that client.

## Vendors

| `ProviderName` | slug | protocols | example model |
| --- | --- | --- | --- |
| `DeepSeek` | `deepseek` | Completions + Responses | `deepseek/deepseek-v4-flash` |
| `Moonshot` | `moonshot` | Completions | `moonshot/kimi-k3` |
| `Zhipu` | `zhipu` | Completions | `zhipu/glm-5.3` |
| `Grok` | `xai` | Responses | `xai/grok-4.6` |
| `OpenAI` | `openai` | Completions + Responses | *(empty catalog — use `list_models()`)* |
| `Custom(name)` | the name you pass | Completions (unless you set `kind`) | `my-proxy/local-llama` |

Anything not covered by a preset: `.base_url(...)` on `ProviderBuilder`.

```rust
let gateway = ProviderBuilder::provider()
    .provider_name(ProviderName::Custom("my-proxy".into()))
    .api_key(api_key)
    .base_url("https://gateway.example.com/v1")
    .build()?;
```

`ProviderName::known_models()` reads that vendor's static catalog without an API key or a client.
`ProviderName::default_model()` is the suggested wire id, when there is one. A user-declared model
is `ModelInfo::declared(id, provider)`: numeric limits stay zero until set, and capability flags
start out supported. `Request::builder().thinking(ThinkingMode::Enabled)` does not set a
reasoning effort; pass `.reasoning_effort(...)` when a level is required.

A vendor with no preset protocol (Anthropic) builds as Completions when you pass `base_url`, so
an OpenAI-compatible proxy does not need a separate protocol override. `Messages` is still
unsupported.

Streaming, tool-calling, thinking/reasoning, request validation, and vendor-specific
`provider_options` are all on the same `Provider` / `Request` types. See the examples rather
than another copy of the API here.

## Examples

None of these require a real API key — they fall back to a placeholder and print errors instead
of panicking:

```sh
cargo run --example router_usage
cargo run --example completions_usage
cargo run --example responses_usage
cargo run --example agent_loop -- "summarize this repository"
```

`router_usage` is the one that matches this README: `ProviderBuilder` + `Router` across DeepSeek,
Zhipu, and a custom gateway.

## Contributing

Bug reports, documentation, new provider presets, new wire protocols, and model catalog updates
are all welcome.

```sh
cargo fmt -- --check
cargo clippy --all-targets
cargo test
```

Keep the public API provider-agnostic: wire types stay in encoder/decoder modules. Add tests
with every behavior change. Examples must stay runnable without a real API key.

Publish by pushing a tag `oven-llm-vX.Y.Z` that matches `version` in this crate's
`Cargo.toml`. That tag runs the oven-llm release workflow, which publishes the crate to
crates.io. The oven repository needs a `CRATES_IO_TOKEN` secret. Oven's own `v*` tags still
only ship the binary.

## License

MIT — see [LICENSE](LICENSE).

# oven-llm

[![crates.io](https://img.shields.io/crates/v/oven-llm.svg)](https://crates.io/crates/oven-llm)
[![docs.rs](https://docs.rs/oven-llm/badge.svg)](https://docs.rs/oven-llm)
[![MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A Rust library for calling LLM providers through one unified, async API.

Source lives in the [oven](https://github.com/guuzaa/oven) repository at `crates/oven-llm` and is published on its own. It does not depend on the oven application crates.

Build a vendor with [`ProviderBuilder`](#one-vendor-providerbuilder), register several of them on
a [`Router`](#several-vendors-router), and name the model as a `vendor/wire-id` slug. A call has
two halves: a `Request` (the conversation) and a `CallOptions` (the model it goes to and how to
ask). Application code talks to the `Provider` trait and a provider-agnostic domain model
(`Request` / `CallOptions` / `Response` / `StreamEvent`). Wire formats stay inside the crate.

> [!WARNING]
> **Status: not production-ready.** This crate is under active development. The public API is
> still evolving and may change without notice, and there are no stability or correctness
> guarantees yet. Please evaluate it before using it in production workloads.

## Install

```sh
cargo add oven-llm
```

## Runtime

The transport is [`reqwest`](https://docs.rs/reqwest), so **a provider must be polled from a Tokio
runtime**. Backoff and timeouts wait on the Tokio timer too. A request issued outside a Tokio
runtime panics instead of returning an error, so run your agent under `#[tokio::main]`.

## The shape of a call

`Request` is the conversation: the system prompt, the messages, the tools. `CallOptions` is the
call: the model it goes to, the sampling parameters, the reasoning effort and any
provider-private field. They are separate arguments, so the same conversation can go out to
another model — or with another effort — without being rebuilt.

```rust
use oven_llm::{CallOptions, ReasoningEffort, Request};

let request = Request::builder()
    .system("be helpful")
    .prompt("Describe what Rust is")
    .build();

let options = CallOptions::new("deepseek/deepseek-v4-flash")
    .reasoning_effort(ReasoningEffort::High);
```

`RequestBuilder::build()` cannot fail. `CallOptions::new(model)` is the only way to make
options: a call always names its model. Nothing else is sent unless you ask for it — an unset
sampling parameter and an unset effort stay off the wire, and the vendor's default stands.

## One vendor (`ProviderBuilder`)

Do not pick a protocol. `ProviderBuilder::provider()` uses that vendor's default protocol
and returns `Box<dyn Provider>`. Use `completions()` / `responses()` to force one.

```rust
use oven_llm::{CallOptions, Provider, ProviderBuilder, ProviderName, Request};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = ProviderBuilder::provider()
        .provider_name(ProviderName::DeepSeek)
        .api_key(std::env::var("DEEPSEEK_API_KEY")?)
        .build()?;

    let request = Request::builder()
        .prompt("Describe what Rust is")
        .build();

    let options = CallOptions::new("deepseek/deepseek-v4-flash");

    let response = provider.complete(&request, &options).await?;
    println!("{}", response.text());
    Ok(())
}
```

`ProviderBuilder::build()` returns a `Result`. An unsupported vendor (for example Anthropic,
which has no implementation yet) is a typed error, not a panic. Known presets keep their base
URL and static catalog; `.add_model(...)` / `.extra_headers(...)` can extend them. A custom
gateway needs `.base_url(...)` and any `ProviderName`, including `Custom(...)`.

Force a single wire protocol only when you need to:

```rust
let provider = ProviderBuilder::completions() // or ::responses()
    .provider_name(ProviderName::DeepSeek)
    .api_key(api_key)
    .build()?;
```

## Several vendors (`Router`)

Register each vendor once. The model in the options decides where the call goes — mixing
vendors is configuration, not a `match` in application code.

```rust
use oven_llm::{CallOptions, Client, Provider, ProviderBuilder, ProviderName, Request, Router};

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
    .prompt("hello")
    .build();

let options = CallOptions::new("zhipu/glm-5.3");

// `Client` implements `Provider`. `complete` and `stream` are the trait methods.
let response = client.complete(&request, &options).await?;
let mut stream = client.stream(&request, &options).await?;
```

`Router` only answers which provider owns a model (`provider`, `qualify`). It does not call
providers and it does not retry. `Client` is that table as a `Provider`.

`register` takes `impl Provider + 'static`, so a concrete client does not need to be boxed first.
`ProviderBuilder::build` still returns `Box<dyn Provider>`, and that box registers as-is.

Retry and timeout are off until you wrap a provider with `RetryingProvider`. When on, transport
errors, rate limits, and HTTP 408/429/5xx are retried. Streaming retries the connection start,
not events already in flight. `list_models` is not retried. The backoff and the timeout wait on
the Tokio timer.

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
| `MiniMax` | `minimax` | Completions | `minimax/MiniMax-M3` |
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
start out supported.

A vendor with no preset protocol (Anthropic) builds as Completions when you pass `base_url`, so
an OpenAI-compatible proxy does not need a separate protocol override. `Messages` is still
unsupported.

## Reasoning effort

`ReasoningEffort` is one axis: `Off`, `Minimal`, `Low`, `Medium`, `High`, `XHigh`, `Max` and
`Budget(tokens)`. An effort that is not set means "no preference" — no reasoning field reaches
the wire and the upstream default stands. `Off` is the explicit opt-out and is not the same
thing. There is no separate thinking switch: it is derived from the effort, so the two
protocols cannot disagree about whether reasoning is on.

```rust
let options = CallOptions::new("zhipu/glm-5.3")
    .reasoning_effort(ReasoningEffort::High);
```

How it reaches the wire depends on the model and on the vendor:

- The **model** says what it can express — `ModelInfo::effort_support`: `Levels` (a set of
  levels), `Toggle` (a switch, no levels), `Fixed` (it reasons, the depth is not yours to
  pick), `Unsupported`, or `Unknown`.
- The **vendor** says which channel the wire uses — `Dialect::Level` (`reasoning_effort`),
  `Dialect::Toggle` (`thinking.type`) or `Dialect::Both`. Zhipu takes the switch and no
  level, OpenAI and DeepSeek take the level and no switch, and a vendor nobody verified yet
  keeps sending both.

A level the model does not have is clamped down to the closest one it has — the set is not a
ladder (DeepSeek has no `medium`), so the clamp compares strength, not position. The request
path never fails on it. A model with a fixed depth gets no reasoning field at all, so a model
that refuses `thinking: disabled` is never told to stop thinking.

Words read from a configuration file: `none` (or `off` / `zero`), `minimal`, `low`, `medium`,
`high`, `xhigh` (or `x-high`), `max`, or a token budget written as a number.

## Sampling

The sampling parameters are not sent unless you set them: the vendor's own default is the right
one. The Kimi family fixes its temperature per model — 1.0 while thinking, 0.6 without — and
rejects any other value, so a request that always carried `temperature: 1.0` would be refused
on every non-thinking Kimi call.

```rust
let options = CallOptions::new("deepseek/deepseek-v4-flash").temperature(0.2);
```

Provider-private fields ride along in the same options and are merged into the wire body:
`.provider_option("top_k", serde_json::json!(40))`.

## Streaming and tool calls

`complete_streaming` drives a streamed call and hands back the assembled response. The callback
sees every event as it arrives; the same events are collected into the response, so the two can
never drift apart.

```rust
use oven_llm::{CallOptions, Request, complete_streaming};

let response = complete_streaming(provider.as_ref(), &request, &options, |event| {
    if let Some(text) = event.text_delta() {
        print!("{text}");
    }
})
.await?;
```

A response that asks for tools says so with `has_tool_calls()`, and the calls themselves come
out typed:

```rust
if response.has_tool_calls() {
    for call in response.tool_calls() {
        let (output, is_error) = run_tool(call.name(), call.input());
        request.message(Message::tool_result(call.id(), output, is_error));
    }
}
```

`stopped_for_tools()` is the strict variant: it also wants the provider to have reported
`tool_calls` as the stop reason. A provider that leaves the reason out, or a call cut short by
the token limit, would then be dropped without the model ever learning that its call failed — so
run whatever came back instead.

`call.arguments()` is the value to run the tool with. It fails with
`ToolArgumentsError::InvalidJson` when the model truncated its argument object — that call
should go back to the model as a tool error rather than run with empty arguments.

Streaming, tool-calling, thinking/reasoning, and request validation are all on the same
`Provider` / `Request` types. See the examples rather than another copy of the API here.

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

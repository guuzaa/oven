//! `oven-llm` Router 路由层示例：按模型 ID 自动派发 provider。
//!
//! 演示：
//! - 用 `ProviderBuilder` 构造已知厂商（DeepSeek / Zhipu）
//! - 再挂一个自定义网关：`name` + `base_url` + key，协议默认 completions
//! - 注册进 `Router`；slug 的 vendor 段决定派发（`my-proxy/...` → 自定义）
//! - 通过 `router.complete` / `router.stream` 自动派发
//! - 未注册模型：`provider()` 返回 `RouterError::UnknownModel`，
//!   `complete` / `stream` 返回 `ProviderError::UnknownModel`
//!
//! 未设置 API key 时使用占位 key，网络调用会失败并打印错误而不会 panic，
//! 因此没有真实 API key/网络也能演示完整的调用流程。

use std::io::{self, Write};
use std::time::Duration;

use futures::StreamExt;
use oven_llm::{
    Delta, ModelId, ModelInfo, ProviderBuilder, ProviderName, Request, Router, RouterError,
    StreamEvent, ThinkingMode,
};

fn api_key(env: &str) -> String {
    std::env::var(env).unwrap_or_else(|_| "sk-placeholder".to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 用统一入口构造两个 provider。
    let deepseek = ProviderBuilder::provider()
        .provider_name(ProviderName::DeepSeek)
        .api_key(api_key("DEEPSEEK_API_KEY"))
        .build()?;
    let zhipu = ProviderBuilder::provider()
        .provider_name(ProviderName::Zhipu)
        .api_key(api_key("ZHIPU_API_KEY"))
        .build()?;

    // 自定义 OpenAI 兼容网关：必须给 base_url；协议默认 completions。
    // 网关若只说 Responses，在 builder 上加 `.kind(ProviderKind::Responses)`。
    let my_proxy = ProviderName::Custom("my-proxy".into());
    let gateway = ProviderBuilder::completions()
        .provider_name(my_proxy.clone())
        .api_key(api_key("MY_PROXY_API_KEY"))
        .base_url("https://gateway.example.com/v1")
        .add_model(ModelInfo::minimal("local-llama", my_proxy))
        .build()?;

    // 2. 注册进 Router。slug 的 vendor 段决定派发。
    let mut router = Router::new();
    router.register(deepseek).register(zhipu).register(gateway);

    // 3. 展示派发解析与未命中错误。
    for id in [
        "deepseek/deepseek-v4-flash",
        "zhipu/glm-5.3",
        "my-proxy/local-llama",
        "xai/grok-4.6",
    ] {
        match router.provider(&ModelId::from(id)) {
            Ok(provider) => println!("{id} -> {}", provider.provider_name()),
            Err(RouterError::UnknownModel(model)) => println!("{id} -> unknown: {model}"),
            Err(err) => println!("{id} -> error: {err}"),
        }
    }

    // 4. 非流式调用（无真实 key 时会打印错误并继续）。
    let request = Request::builder()
        .model("deepseek/deepseek-v4-flash")
        .prompt("用一句话介绍 Rust 语言。")
        .temperature(0.01)
        .thinking(ThinkingMode::Disabled)
        .build()?;
    match router.complete(&request).await {
        Ok(response) => println!("complete() -> {}", response.text()),
        Err(err) => eprintln!("complete() failed (expected without a real API key): {err}"),
    }

    // 5. 流式调用。
    tokio::time::sleep(Duration::from_secs(1)).await;
    match router.stream(&request).await {
        Ok(mut stream) => {
            print!("stream(): ");
            while let Some(event) = stream.next().await {
                match event {
                    Ok(StreamEvent::ContentBlockDelta {
                        delta: Delta::TextDelta { text },
                        ..
                    }) => {
                        print!("{text}");
                        io::stdout().flush()?;
                    }
                    Ok(_) => {}
                    Err(err) => {
                        eprintln!("\nstream() error (expected without a real API key): {err}");
                        break;
                    }
                }
            }
            println!();
        }
        Err(err) => eprintln!("stream() failed to start (expected without a real API key): {err}"),
    }

    Ok(())
}

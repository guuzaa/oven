//! `oven-llm` CompletionsProvider 基础用法示例。
//!
//! 演示：
//! - 使用 `ModelId` 构建 provider 无关的 `Request`
//! - 创建带静态模型目录的 `OpenAICompatProvider::deepseek`
//! - 直接调用 `complete` / `stream`；已知模型会由 provider 自动校验，
//!   未命中目录的模型则按宽松策略透传
//! - 演示 `list_models`、非流式调用、流式调用并打印 `Delta::TextDelta` 内容
//!
//! 运行：
//! ```sh
//! ZHIPU_API_KEY=sk-xxx cargo run --example basic_usage
//! ```
//! 未设置 `ZHIPU_API_KEY` 时会使用一个占位 key，网络调用会失败——
//! 本示例会打印错误并继续，而不会 panic，因此即使没有真实 API key/网络也能
//! 演示完整的调用流程。

use std::io::Write;
use std::{io, time::Duration};

use oven_llm::{CompletionsProvider, Delta, Message, Provider, Request, StreamEvent, ThinkingMode};

async fn provider_example(provider: Box<dyn Provider>, request: &Request) {
    // 3. 演示 list_models（可能因缺少真实 API key/网络而失败，打印错误后继续）。
    match provider.list_models().await {
        Ok(models) => {
            println!("list_models returned {} models", models.len());
            for model in &models {
                println!("  - {}", model.id);
            }
        }
        Err(err) => eprintln!("list_models failed (expected without a real API key): {err}"),
    }

    // 4. 演示非流式 complete 调用。
    match provider.complete(request).await {
        Ok(response) => {
            println!(
                "complete() succeeded: id={} stop_reason={:?}",
                response.id, response.stop_reason
            );
            let thinking = response.thinking();
            if !thinking.is_empty() {
                println!("  \x1b[90mthinking: {thinking}\x1b[0m");
            }
            let text = response.text();
            if !text.is_empty() {
                println!("  text: {text}");
            }
        }
        Err(err) => eprintln!("complete() failed (expected without a real API key): {err}"),
    }

    std::thread::sleep(Duration::from_secs(1));
    // 5. 演示流式 stream 调用，逐块打印 Delta::TextDelta 内容。
    match provider.stream(request).await {
        Ok(mut event_stream) => {
            print!("stream() text: ");
            use futures::StreamExt;
            while let Some(event) = event_stream.next().await {
                match event {
                    Ok(StreamEvent::ContentBlockDelta {
                        delta: Delta::ThinkingDelta { thinking },
                        ..
                    }) => {
                        print!("\x1b[90m{thinking}\x1b[0m");
                        io::stdout().flush().unwrap();
                    }
                    Ok(StreamEvent::ContentBlockDelta {
                        delta: Delta::TextDelta { text },
                        ..
                    }) => {
                        print!("{text}");
                        io::stdout().flush().unwrap();
                    }
                    Ok(StreamEvent::MessageDelta {
                        usage: Some(usage), ..
                    }) => {
                        println!("\nusage: {:?}", usage)
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
}

#[tokio::main]
async fn main() {
    // 1. 创建 DeepSeek provider（从环境变量读取 API key，未设置时使用占位值）。
    let api_key = std::env::var("ZHIPU_API_KEY").unwrap_or_else(|_| "sk-placeholder".to_string());
    let provider = Box::new(CompletionsProvider::zhipu(api_key));

    // 2. 构建请求。Provider 会为其静态模型目录中命中的 ID 自动执行能力校验。
    let request = Request::builder()
        .model("glm-4.5-air")
        .message(Message::user_text("用一句话介绍一下 Rust 语言。"))
        .temperature(0.0)
        .thinking(ThinkingMode::Enabled)
        .build()
        .expect("model is set");

    provider_example(provider, &request).await
}

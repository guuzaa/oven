//! `oven-llm` Responses API 使用示例。
//!
//! 演示：
//! - 使用 `OpenAIResponsesProvider::deepseek` 预设创建 provider
//! - `list_models` 动态模型发现
//! - 非流式 `complete` 调用（打印 thinking / text / usage / tool_use）
//! - 流式 `stream` 调用（thinking 与 text 增量实时打印）
//! - 工具调用循环：`function_call` → 执行工具 → `function_call_output` 回传，
//!   直到模型不再请求调用工具
//!
//! 运行：
//! ```sh
//! DEEPSEEK_API_KEY=sk-xxx cargo run --example responses_usage
//! ```
//!
//! 未设置 `DEEPSEEK_API_KEY` 时会使用一个占位 key，网络调用会失败——本示例会
//! 打印错误并继续，而不会 panic，因此即使没有真实 API key/网络也能演示完整的
//! 调用流程。

use std::sync::Arc;

use std::io::{self, Write};

use oven_llm::{
    CallOptions, Message, Provider, ProviderBuilder, ProviderName, Request, Tool,
    complete_streaming,
};
use serde_json::{Value, json};

#[tokio::main]
async fn main() {
    // 1. 创建 provider（DeepSeek 预设；`ResponsesProvider::new` 对无预设的厂商返回
    //    `Err`，`with_base_url` / `with_models` 可自定义服务商）。
    let api_key =
        std::env::var("DEEPSEEK_API_KEY").unwrap_or_else(|_| "sk-placeholder".to_string());
    let provider = ProviderBuilder::responses()
        .provider_name(ProviderName::DeepSeek)
        .api_key(api_key)
        .build()
        .expect("DeepSeek responses API");
    println!("provider: {:?}", provider.provider_name());

    // 2. 动态模型发现。可能因缺少真实 API key/网络而失败，打印错误后继续。
    match provider.list_models().await {
        Ok(models) => {
            println!("list_models returned {} models", models.len());
            for model in &models {
                println!("  - {}", model.id);
            }
        }
        Err(err) => eprintln!("list_models failed (expected without a real API key): {err}"),
    }

    // 3. 非流式 complete 调用。
    let request = Request::builder()
        .message(Message::user_text("用一句话介绍 Rust 语言。"))
        .build();
    let options = CallOptions::new("deepseek-v4-flash").temperature(0.0);

    match provider.complete(&request, &options).await {
        Ok(response) => {
            println!(
                "complete() succeeded: id={} stop_reason={:?} usage={:?}",
                response.id, response.stop_reason, response.usage
            );
            let thinking = response.thinking();
            if !thinking.is_empty() {
                println!("  \x1b[90mthinking: {thinking}\x1b[0m");
            }
            let text = response.text();
            if !text.is_empty() {
                println!("  text: {text}");
            }
            for call in response.tool_calls() {
                println!(
                    "  tool_use: {} {}({})",
                    call.id(),
                    call.name(),
                    call.input()
                );
            }
        }
        Err(err) => eprintln!("complete() failed (expected without a real API key): {err}"),
    }

    // 4. 流式 stream + 工具调用循环。默认 tool_choice 为 Auto（模型自行决定
    //    是否调用工具）；也可通过 `.tool_choice(ToolChoice::Tool("get_weather"))`
    //    强制指定必须调用的工具。
    run_tool_loop(provider.as_ref()).await;
}

async fn collect_streamed_response<P: Provider + ?Sized>(
    provider: &P,
    request: &Request,
    options: &CallOptions,
) -> Result<oven_llm::Response, oven_llm::ProviderError> {
    print!("  assistant: ");
    io::stdout().flush().expect("flush stdout");

    let response = complete_streaming(provider, request, options, |event| {
        if let Some(thinking) = event.thinking_delta() {
            print!("\x1b[90m{thinking}\x1b[0m");
        } else if let Some(text) = event.text_delta() {
            print!("{text}");
        } else {
            return;
        }
        io::stdout().flush().expect("flush stdout");
    })
    .await?;
    println!();

    Ok(response)
}

/// 流式工具调用循环：把每次流式响应追加为 assistant 消息，若响应请求调用工具
/// 则执行并把结果以 `function_call_output` 回传，直到模型结束本轮生成。
async fn run_tool_loop<P: Provider + ?Sized>(provider: &P) {
    let mut request = Request::builder()
        .message(Message::user_text(
            "请调用 get_weather 工具查询杭州今天的天气，然后告诉我结果。",
        ))
        .tools(vec![weather_tool()])
        .build();
    let options = CallOptions::new("deepseek-v4-flash").temperature(0.0);

    loop {
        let response = match collect_streamed_response(provider, &request, &options).await {
            Ok(response) => response,
            Err(err) => {
                eprintln!("stream() failed (expected without a real API key): {err}");
                return;
            }
        };

        // 关键顺序：先提交完整的 assistant 消息（含 function_call），再执行工具
        // 并追加 function_call_output，保证多轮 wire 顺序正确。
        request.message(Message::assistant(response.content.clone()));

        if !response.has_tool_calls() {
            println!("tool loop finished");
            return;
        }

        for call in response.tool_calls() {
            println!("  tool call: {}({})", call.name(), call.input());
            let (output, is_error) = execute_demo_tool(call.name(), call.input());
            request.message(Message::tool_result(call.id(), output, is_error));
        }
    }
}

/// 演示用工具：不访问真实天气服务，返回固定数据。
fn weather_tool() -> Tool {
    Tool {
        name: "get_weather".to_string(),
        description: Some("查询指定城市的当前天气。".to_string()),
        input_schema: Arc::new(json!({
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "城市名，例如 Hangzhou"
                }
            },
            "required": ["location"],
            "additionalProperties": false
        })),
    }
}

/// 执行演示工具并返回 `(输出文本, 是否错误)`。
fn execute_demo_tool(name: &str, input: &Value) -> (String, bool) {
    match name {
        "get_weather" => {
            let location = input
                .get("location")
                .and_then(Value::as_str)
                .unwrap_or("未知城市");
            (format!("{location} 今天晴，26°C，适合出门。"), false)
        }
        other => (format!("unknown tool: {other}"), true),
    }
}

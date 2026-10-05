//! 一个带工具调用的流式 coding agent。
//!
//! 运行方式：
//! `DEEPSEEK_API_KEY=sk-xxx cargo run --example agent_loop -- "为这个项目补充 README"`
//!
//! 默认将当前目录视为工作区；可通过 `CODING_AGENT_ROOT` 限制 agent 可读写的根目录。

use std::{
    env,
    error::Error,
    fs,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

use futures::StreamExt;
use oven_llm::{
    ContentBlock, Delta, Message, Provider, ProviderBuilder, ProviderName, Request, StopReason,
    StreamCollector, StreamEvent, Tool,
};
use serde_json::{Value, json};

type ExampleResult<T> = Result<T, Box<dyn Error>>;

const MAX_FILE_BYTES: u64 = 64 * 1024;

#[tokio::main]
async fn main() -> ExampleResult<()> {
    let workspace_root = coding_workspace_root()?;
    let task = coding_task();
    let api_key = env::var("DEEPSEEK_API_KEY").unwrap_or_else(|_| "sk-placeholder".to_string());
    let provider = ProviderBuilder::completions()
        .provider_name(ProviderName::DeepSeek)
        .api_key(api_key)
        .build()?;

    println!("coding workspace: {}", workspace_root.display());
    println!("task: {task}");

    let mut request = Request::builder()
        .model("deepseek-v4-flash")
        .system(format!(
            "You are a careful coding agent. Your workspace root is {}. \
             Use read_file before changing a file. Use write_file only for files inside that root. \
             Make the requested change, then give a concise final summary.",
            workspace_root.display()
        ))
        .prompt(task)
        .tools(coding_tools())
        .temperature(0.1)
        .thinking(oven_llm::ThinkingMode::Enabled)
        .build()
        .expect("model is set");

    loop {
        let response = collect_streamed_response(provider.as_ref(), &request).await?;
        let requests_tools =
            response.stop_reason == Some(StopReason::ToolUse) && response.has_tool_use();
        println!("usage: {:?}", response.usage);

        // 关键顺序：先提交完整的 assistant 消息（含 tool_use），再执行工具并追加结果。
        // OpenAI 兼容接口要求 role=tool 紧跟在发起该调用的 assistant 消息之后。
        request.message(Message::assistant(response.content.clone()));

        if !requests_tools {
            break;
        }

        for block in response.tool_uses() {
            let ContentBlock::ToolUse {
                id, name, input, ..
            } = block
            else {
                continue;
            };
            println!("tool: {name}({input})");
            let (output, is_error) = execute_tool(&workspace_root, name, input);
            request.message(Message::tool_result(id, output, is_error));
        }
    }

    Ok(())
}

/// 消费一次流式响应，利用 [`StreamCollector`] 拼装完整的 assistant [`Response`]。
///
/// 文本和 thinking 增量会实时打印到终端；工具参数则静默累积。
async fn collect_streamed_response<P: Provider + ?Sized>(
    provider: &P,
    request: &Request,
) -> ExampleResult<oven_llm::Response> {
    let mut stream = provider.stream(request).await?;
    let mut collector = StreamCollector::new();

    print!("\nassistant: ");
    io::stdout().flush()?;

    while let Some(event) = stream.next().await {
        let event = event?;

        if let StreamEvent::ContentBlockDelta { delta, .. } = &event {
            match delta {
                Delta::ThinkingDelta { thinking } => {
                    print!("\x1b[90m{thinking}\x1b[0m");
                    io::stdout().flush()?;
                }
                Delta::TextDelta { text } => {
                    print!("{text}");
                    io::stdout().flush()?;
                }
                Delta::InputJsonDelta { .. } => {}
            }
        }

        collector.push(&event);
    }
    println!();

    Ok(collector.finish()?)
}

fn coding_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "read_file".to_string(),
            description: Some("Read a UTF-8 text file from the coding workspace.".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path from the workspace root" }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
        Tool {
            name: "write_file".to_string(),
            description: Some(
                "Create or replace a UTF-8 text file in the coding workspace.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path from the workspace root" },
                    "content": { "type": "string", "description": "Complete replacement file content" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        },
    ]
}

fn execute_tool(workspace_root: &Path, name: &str, input: &Value) -> (String, bool) {
    match name {
        "read_file" => {
            let result = tool_path(workspace_root, input).and_then(|path| {
                let metadata = fs::metadata(&path).map_err(|error| error.to_string())?;
                if metadata.len() > MAX_FILE_BYTES {
                    return Err(format!(
                        "{} is {} bytes; only files up to {MAX_FILE_BYTES} bytes may be read",
                        path.display(),
                        metadata.len()
                    ));
                }
                fs::read_to_string(&path).map_err(|error| error.to_string())
            });
            result_to_tool_output(result)
        }
        "write_file" => {
            let result = (|| {
                let path = tool_path(workspace_root, input)?;
                let content = input
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "missing string field 'content'".to_string())?;
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                fs::write(&path, content).map_err(|error| error.to_string())?;
                Ok(format!(
                    "Wrote {} bytes to {}",
                    content.len(),
                    path.display()
                ))
            })();
            result_to_tool_output(result)
        }
        other => (format!("unknown tool: {other}"), true),
    }
}

fn result_to_tool_output(result: Result<String, String>) -> (String, bool) {
    match result {
        Ok(output) => (output, false),
        Err(error) => (error, true),
    }
}

/// 仅接受普通相对路径，避免工具通过 `..` 或绝对路径离开工作区。
fn tool_path(workspace_root: &Path, input: &Value) -> Result<PathBuf, String> {
    let relative_path = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing string field 'path'".to_string())?;
    let relative_path = Path::new(relative_path);

    if relative_path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(
            "path must be a non-empty relative path without '.' or '..' segments".to_string(),
        );
    }

    Ok(workspace_root.join(relative_path))
}

fn coding_workspace_root() -> ExampleResult<PathBuf> {
    let root = env::var_os("CODING_AGENT_ROOT")
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?);
    Ok(root.canonicalize()?)
}

fn coding_task() -> String {
    let task = env::args().skip(1).collect::<Vec<_>>().join(" ");
    if task.is_empty() {
        "Inspect this Rust project and suggest one small, useful improvement. Do not write files unless it is necessary to complete the improvement.".to_string()
    } else {
        task
    }
}

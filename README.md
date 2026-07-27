# Oven

A coding agent that runs in your terminal. It connects to an LLM, reads
and writes files in your project, runs bash commands, and helps you get
things done.

## Usage

```bash
# One-shot: ask a question and get an answer
oven "what does this project do?"

# Interactive mode (opens a TUI)
oven

# With a specific model and API key
OVEN_MODEL=deepseek-v4-flash OVEN_DEEPSEEK_API_KEY=sk-xxx oven

# Resume a previous session
oven --session my-session

# Work in a different directory
oven --root /path/to/project
```

## How it works

Oven runs inside your project directory. It:

1. Takes your prompt and sends it to an LLM provider
2. The LLM can read files, write files, and run shell commands
3. Oven streams the response back in real time

## Supported providers

Oven speaks the OpenAI chat completions format, so it works with any
provider that supports it:

| Provider       | Model prefix     | Env var for API key          |
|----------------|------------------|------------------------------|
| OpenAI         | `gpt-*`, `o1-*`  | `OPENAI_API_KEY`             |
| DeepSeek       | `deepseek-*`     | `DEEPSEEK_API_KEY`           |
| Zhipu (智谱)   | `glm-*`          | `OVEN_ZHIPU_API_KEY`         |
| Moonshot (Kimi)| `kimi-*`         | `MOONSHOT_API_KEY`           |
| Anthropic*     | `claude-*`       | `ANTHROPIC_API_KEY`          |

\* Anthropic requires an OpenAI-to-Anthropic proxy (e.g.
`ANTHROPIC_BASE_URL` pointing to one).

Set `OVEN_MODEL` to pick the model. Override the base URL with
`OVEN_BASE_URL` (or the provider-specific vars like `ANTHROPIC_BASE_URL`,
`OPENAI_BASE_URL`) if you need a proxy or self-hosted endpoint.

## Configuration

Put a `.oven.yaml` in your project root, or configure globally at
`~/.config/oven/config.yaml`. Available options:

```yaml
provider:
  model: deepseek-v4-flash
  base_url: https://api.deepseek.com
  api_key: sk-xxx

request_timeout_secs: 60
max_retries: 2
base_backoff_ms: 500

skills:
  - files
  - bash

mcps:
  # MCP server declarations (transport support coming)
```

If no `.oven.yaml` exists, Oven uses sensible defaults.

## Modes

**One-shot** — pass a prompt as arguments. Oven replies and exits.

```bash
oven "what is love?"
```

**Interactive** — run without arguments for a TUI. Type your prompt,
press Enter to send. `Esc` cancels the current request, `Ctrl-C` quits.

```bash
oven
```

**Piped** — pipe input in:

```bash
echo "summarize this" | oven
```

## Sessions

Use `--session <id>` to persist conversation history. Next time you
run with the same id, the history is loaded and continued.

```bash
oven --session my-project "start implementing the parser"
# later…
oven --session my-project "add error handling to the parser"
```

Sessions are stored as JSONL files in the platform's data directory.

## Slash commands

In both one-shot and interactive modes:

| Command        | What it does                  |
|----------------|-------------------------------|
| `/clear`       | Clear conversation history    |
| `/help`        | Show available commands       |
| `/exit`        | Exit the agent                |

## Building from source

```bash
cargo build -r
./target/release/oven
```

## License

MIT

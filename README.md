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

Put a `.oven.toml` in your project root, or configure globally at
`$XDG_CONFIG_HOME/oven/config.toml` (default `~/.config/oven/config.toml`).
On first run Oven creates the global config as a template if it is missing.
Available options:

```toml
request_timeout_secs = 60
max_retries = 2
base_backoff_ms = 500

skills = []
tools = ["file_read", "file_write", "bash"]

[provider]
model = "deepseek-v4-flash"
base_url = "https://api.deepseek.com"
api_key = "sk-xxx"

[mcps]
# MCP server declarations (transport support coming)
```

If neither file exists, Oven uses sensible defaults. Sessions are stored as
JSONL under `$XDG_DATA_HOME/oven/sessions/` (default
`~/.local/share/oven/sessions/`).

`skills` opts into guidance modules that inject instructions into the system
prompt; no skills are bundled yet, so entries are accepted and currently
skipped. `tools` selects which capabilities the agent can actually invoke; an
empty `tools:` list mounts the built-in default set (`file_read`,
`file_write`, `bash`). The two are independent.

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

Use `--session <id>` to resume an existing session; its history is
loaded and continued. A new session always gets an auto-generated id
(uuid v7) that the app manages internally, so you never provide an id
for a new chat.

```bash
oven --session my-project "continue the previous chat"
```

Sessions are stored as JSONL files in the platform's data directory.

## Slash commands

In both one-shot and interactive modes:

| Command        | What it does                  |
|----------------|-------------------------------|
| `/clear`       | Clear the screen and start a new chat (switches to a new uuid v7 session; the old file is kept) |
| `/exit`        | Exit the agent                |

## Building from source

```bash
cargo build -r
./target/release/oven
```

## License

MIT

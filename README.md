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
oven -C /path/to/project
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

tools = ["file_read", "file_write", "bash"]

[provider]
model = "deepseek-v4-flash"
base_url = "https://api.deepseek.com"
api_key = "sk-xxx"

[mcps]
# MCP servers, e.g.
# [mcps.filesystem]
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-filesystem", "/abs/path"]
# [mcps.filesystem.env]
# FOO = "bar"
# ...or a remote streamable HTTP server:
# [mcps.remote]
# url = "https://example.com/mcp"
# [mcps.remote.headers]
# Authorization = "Bearer sk-xxx"
```

If neither file exists, Oven uses sensible defaults. Sessions are stored as
JSONL under `$XDG_DATA_HOME/oven/sessions/` (default
`~/.local/share/oven/sessions/`).

Instruction files (`AGENTS.md` / `CLAUDE.md`) are loaded from the user config
dir (`~/.config/oven/`) and the workspace root, and their contents are injected
into the system prompt. Both files are loaded when present (`AGENTS.md` before
`CLAUDE.md`), user-level instructions first, project instructions after;
missing, empty, or unreadable files are ignored.

Skills are guidance modules loaded from the filesystem. Each skill is a
directory containing a `SKILL.md` file; its `description:` YAML frontmatter
is injected into the system prompt as `- **<name>**: <description>`, and the
full document body is loaded on demand by the `skill_read` tool. Skills are
searched in `~/.local/share/oven/skills/<name>/` (user-wide) and
`.oven/skills/<name>/` in the project (project skills override user skills
with the same name). `tools` selects which capabilities the agent can
actually invoke; an empty `tools:` list mounts the built-in default set
(`file_read`, `file_write`, `bash`). MCP servers declared under `mcps:` are
connected at
startup — stdio servers via a spawned child process (`command`/`args`/`env`),
remote servers via a streamable HTTP endpoint (`url` plus optional `headers`).
Each server's tools are discovered via `tools/list` and mounted for the agent
under a `<server_id>_<tool_name>` name (for example, the `filesystem` server's
`read_file` tool becomes `filesystem_read_file`). Connection or negotiation
failures abort startup with a clear error.

## Modes

**One-shot** — pass a prompt with `--query`. Oven replies and exits.

```bash
oven --query "what is love?"
oven -Q "what is love?"
```

**Interactive** — run without arguments for a TUI. Type your prompt,
press Enter to send. While a request is running, `Esc` cancels it (or pops
a queued message back into the input if any are queued); once idle, `Esc`
rewinds the last exchange and restores your message to the input — keep
pressing to rewind further, down to the first message. `Ctrl-C` quits.

```bash
oven
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
After an interactive session exits, its id is printed so you can resume it
later with `oven --session <id>` — but only when the session actually has
conversation content (an empty session prints nothing and creates no file).

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

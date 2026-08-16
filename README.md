# Oven

A terminal coding agent. Oven connects to an LLM, reads and writes files in
your project, runs shell commands, and helps you get things done.

## Usage

```bash
# One-shot query
oven "what does this project do?"
oven -Q "what is love?"

# Interactive TUI
oven

# Pick model / API key
OVEN_MODEL=deepseek-v4-flash OVEN_API_KEY=sk-xxx oven

# Resume a session
oven --session my-session
oven -c                 # resume the most recent session in this directory

# Work in a different directory
oven -C /path/to/project
```

Run `oven --help` for the full CLI.

## How it works

Oven runs inside your project directory and speaks the OpenAI chat
completions format, so any compatible provider works (OpenAI `gpt-*`,
DeepSeek `deepseek-*`, Zhipu `glm-*`, Kimi `kimi-*`). The model can read
and write files and run shell commands; responses stream back in real time.

## Configuration

Config lives in `.oven.toml` at the project root, or globally at
`~/.config/oven/config.toml` (created as a template on first run). Env vars
`OVEN_MODEL`, `OVEN_API_KEY`, and `OVEN_BASE_URL` override it.

```toml
tools = ["file_read", "file_write", "bash"]

[provider]
model = "deepseek-v4-flash"
base_url = "https://api.deepseek.com"
api_key = "sk-xxx"
```

- `tools` — capabilities the agent can invoke (`file_read`, `file_write`,
  `bash`, `glob`, `grep`); an empty list means the defaults.
- `[mcps]` — MCP servers: stdio (`command`/`args`/`env`) or remote
  streamable HTTP (`url`/`headers`); their tools are mounted as
  `<server>_<tool>`.
- `AGENTS.md` / `CLAUDE.md` from `~/.config/oven/` and the project root are
  injected into the system prompt.
- Skills live in `SKILL.md` directories under `~/.local/share/oven/skills/`
  and `.oven/skills/` (project wins).
- Sessions are JSONL files under `~/.local/share/oven/sessions/`.

## Interactive mode

Type a prompt and press Enter to send. While running, `Esc` cancels; when
idle, `Esc` rewinds the last exchange and restores your message. `Ctrl-C`
quits, `Shift+Tab` toggles plan mode. The TUI shows the current todo list,
model, and working directory; mouse selection copies automatically. After
exiting, the session id is printed so you can resume with `--session <id>`.

## Slash commands

| Command  | What it does |
|----------|--------------|
| `/clear` | New chat (new session, old file kept) |
| `/exit`  | Quit |
| `/model` | Switch model: `/model <id> [none\|low\|medium\|high]` |
| `/setup` | Configure provider: `/setup name=... kind=... api_key=...` |
| `/plan`  | Toggle plan mode: `/plan [on\|off]` |

## Build from source

```bash
cargo build -r
./target/release/oven
```

## License

MIT

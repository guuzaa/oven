# Oven

A terminal coding agent. Oven connects to an LLM, reads and writes files in
your project, runs shell commands, and helps you get things done.

> [!WARNING]
> **Status: not production-ready.**

## Install

macOS / Linux:

```bash
curl -fsSL https://oven.paulden.site/install.sh | bash
```

Windows (PowerShell):

```powershell
irm https://oven.paulden.site/install.ps1 | iex
```

Restart your terminal, then verify with `oven --help`.

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

## Logs

Oven writes a rotating log to `~/.oven/logs/oven.log` (10 MiB per file, one backup). The TUI never prints logs to the terminal. Increase verbosity with `OVEN_LOG=debug` (or `RUST_LOG`).

## How it works

Oven runs inside your project directory and speaks the OpenAI chat
completions format, so any compatible provider works. The model can read and
write files, run shell commands, and delegate jobs to subagents
([`docs/subagents.md`](docs/subagents.md)).

## Configuration

Config lives in `.oven.toml` at the project root, or globally at
`~/.oven/config.toml` (created as a template on first run). Env vars
`OVEN_MODEL`, `OVEN_API_KEY`, and `OVEN_BASE_URL` override it.

```toml
tools = ["file_read", "file_write", "bash"]

# Fraction of the model's context window that triggers automatic history
# compaction after a turn completes. 0 disables auto-compaction; /compact
# still works. Ignored when the active model's window size is unknown.
compact_threshold = 0.8

# Provider round trips one turn may take before the loop asks to continue.
max_iters = 200

[subagents]
enabled = true
max_concurrent = 4
max_iters = 60

[provider]
name = "deepseek"

[providers.deepseek]
model = "deepseek-v4-flash"
base_url = "https://api.deepseek.com"
api_key = "sk-xxx"
reasoning_effort = "high"

[providers.xai]
api_key = "xai-xxx"
reasoning_effort = "low"
```

- `tools` — capabilities the agent can invoke (`file_read`, `file_write`,
  `bash`, `glob`, `grep`); an empty list means the defaults.
- `max_iters` — provider round trips one turn may take before the loop asks
  whether to continue; `[subagents]` tunes delegation (`enabled`,
  `max_concurrent`, and the subagents' own `max_iters`).
- `[mcps]` — MCP servers: stdio (`command`/`args`/`env`) or remote
  streamable HTTP (`url`/`headers`); their tools are mounted as
  `<server>_<tool>`.
- `AGENTS.md` / `CLAUDE.md` from `~/.oven/` and the project root are
  injected into the system prompt.
- Skills live in `SKILL.md` directories under `~/.oven/skills/`
  and `.oven/skills/` (project wins).
- Sessions are JSONL files under `~/.oven/sessions/`.

## Interactive mode

Type a prompt and press Enter to send. `Esc` cancels a running turn, or
rewinds the last exchange when idle. `Shift+Tab` toggles plan or ask mode and
`Ctrl-C` quits.

## Slash commands

| Command  | What it does |
|----------|--------------|
| `/clear` | New chat (new session, old file kept) |
| `/exit`  | Quit |
| `/model` | Switch model: `/model <id> [none\|low\|medium\|high]` |
| `/setup` | Configure provider: `/setup name=... api_key=...` |
| `/plan`  | Toggle plan mode: `/plan [on\|off]` |
| `/agents` | List subagents; `/agents <name>` opens one, `/agents stop <name\|all>` stops them |
| `/compact`  | Compact conversation history into a summary; auto-compaction triggers at `compact_threshold`. |

## Build from source

```bash
cargo build -r
./target/release/oven
```

## License

MIT

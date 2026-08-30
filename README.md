# Oven

A terminal coding agent. Oven connects to an LLM, reads and writes files in
your project, runs shell commands, and helps you get things done.

> [!WARNING]
> **Status: not production-ready.**

## Install

Prebuilt binaries are attached to every release tag. Grab the installer for
your platform — it downloads the matching archive, puts `oven` in
`~/.oven/bin` (`%USERPROFILE%\.oven\bin` on Windows), and adds that directory
to your `PATH`.

Linux / macOS — one-liner (latest release):

```bash
curl -fsSL https://raw.githubusercontent.com/guuzaa/oven/master/scripts/install.sh | bash
```

Or pin a specific version:

```bash
curl -fsSL https://raw.githubusercontent.com/guuzaa/oven/master/scripts/install.sh | bash -s v0.0.1
```

Or run the installer from a checkout:

```bash
./scripts/install.sh            # latest release
./scripts/install.sh v0.0.1     # or a specific tag
```

Windows (x86_64) — PowerShell one-liner (latest release):

```powershell
irm https://raw.githubusercontent.com/guuzaa/oven/master/scripts/install.ps1 | iex
```

Pin a specific version:

```powershell
$env:OVEN_VERSION='v0.0.1'; irm https://raw.githubusercontent.com/guuzaa/oven/master/scripts/install.ps1 | iex
```

Or run the installer from a checkout:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install.ps1 v0.0.1
```

The installer defaults to the latest release; pass a tag argument or set the
`OVEN_VERSION` environment variable to pin one (the `v` prefix is optional).
Restart your terminal after installing, then verify with `oven --help`.
Prebuilt binaries cover Linux x86_64/arm64 (musl), macOS x86_64/arm64, and
Windows x86_64.

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
`~/.oven/config.toml` (created as a template on first run). Env vars
`OVEN_MODEL`, `OVEN_API_KEY`, and `OVEN_BASE_URL` override it.

```toml
tools = ["file_read", "file_write", "bash"]

[provider]
name = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
base_url = "https://api.deepseek.com"
api_key = "sk-xxx"

[providers.xai]
api_key = "xai-xxx"
```

- `tools` — capabilities the agent can invoke (`file_read`, `file_write`,
  `bash`, `glob`, `grep`); an empty list means the defaults.
- `[mcps]` — MCP servers: stdio (`command`/`args`/`env`) or remote
  streamable HTTP (`url`/`headers`); their tools are mounted as
  `<server>_<tool>`.
- `AGENTS.md` / `CLAUDE.md` from `~/.oven/` and the project root are
  injected into the system prompt.
- Skills live in `SKILL.md` directories under `~/.oven/skills/`
  and `.oven/skills/` (project wins).
- Sessions are JSONL files under `~/.oven/sessions/`.

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
| `/setup` | Configure provider: `/setup name=... api_key=...` |
| `/plan`  | Toggle plan mode: `/plan [on\|off]` |

## Build from source

```bash
cargo build -r
./target/release/oven
```

## License

MIT

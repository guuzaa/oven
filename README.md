<div align="center">

<img src="./scripts/worker/public/icon.svg" alt="Oven" width="112" height="112" />

# Oven

A toy coding agent for joy only.

Oven reads and edits your files, runs shell commands, delegates to subagents and remembers your project between sessions.

<p>
<img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-ff8550" alt="Platform: macOS, Linux, Windows" />
<img src="https://img.shields.io/badge/rust-1.92%2B-ff8550" alt="Rust 1.92+" />
<a href="https://github.com/guuzaa/oven/releases/latest"><img src="https://img.shields.io/github/v/release/guuzaa/oven?label=release&color=ff8550" alt="Latest release" /></a>
<a href="https://github.com/guuzaa/oven/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-ff8550" alt="License: MIT" /></a>
</p>

[Install](#install) · [Usage](#usage) · [Configuration](#configuration) ·
[Interactive mode](#interactive-mode) · [Build from source](#build-from-source) ·
[License](#license)

</div>

> [!WARNING]
> **Status: not production-ready.**

## Install

macOS / Linux:

```bash
curl -fsSL https://oven.paulden.site/install | bash
```

Windows (PowerShell):

```powershell
irm https://oven.paulden.site/install | iex
```

> [!NOTE]
> `/install` serves the script matching the caller (`PowerShell/` in the
> `User-Agent` selects `install.ps1`); `/install.sh` and `/install.ps1` still work.

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

# Run without durable memory: no memory tools, nothing recalled into the prompt
oven --amnesia

# List, add and remove provider models without starting the TUI
oven model
oven model add            # asks for what the flags leave out
oven model rm xai
```

Run `oven --help` for the full CLI.

## How it works

Oven runs inside your project directory and speaks the OpenAI chat completions
format, so any compatible provider works. The model can read and write files,
run shell commands, and delegate jobs to subagents
([`docs/subagents.md`](docs/subagents.md)).

- Durable memory under `.oven/memory` / `~/.oven/memory`, injected into the
  system prompt and managed with `memory_read`, `memory_write`,
  `memory_forget` or `/memory` ([`docs/memory.md`](docs/memory.md))
- MCP servers mounted as `<server>_<tool>` ([`docs/oven-app.md`](docs/oven-app.md))
- Sessions as JSONL files under `~/.oven/sessions/`, resumed with `-c`
- The architecture is described in [`docs/architectures.md`](docs/architectures.md)

## Configuration

Config lives in `.oven.toml` at the project root, or globally at
`~/.oven/config.toml` (created as a template on first run). Env vars
`OVEN_MODEL`, `OVEN_API_KEY`, and `OVEN_BASE_URL` override it.

`oven model` manages that file without starting the TUI:

```bash
oven model                       # every configured and shipped model
oven model ls myproxy            # just one provider
oven model add                   # interactive: provider, endpoint, key, model, limits
oven model rm myproxy/my-model   # drop one model; `oven model rm myproxy` drops the rest
```

`oven model add` writes to `~/.oven/config.toml` and, unless `--activate` is
passed, leaves the current model selected. Every question is also a flag, so a
script needs no answers at all — a missing required one fails naming it:

```bash
oven model add myproxy --base-url https://proxy.example/v1 --api-key sk-xxx \
  --model my-model --context-window 200k --max-output-tokens 8k --no-vision
```

| Flag | Meaning |
| --- | --- |
| `--base-url` | endpoint; required for a vendor oven does not ship |
| `--protocol` | `completions` (default) or `responses`; unknown vendors only |
| `--api-key` / `--api-key-env` | save a key, or read `OVEN_API_KEY` instead |
| `--model` | wire id to add |
| `--context-window`, `--max-output-tokens` | limits, as `200k` or `1m` |
| `--no-tools`, `--no-vision` | capabilities the model lacks |
| `--reasoning-effort` | `none`, `low`, `medium` or `high` |
| `--activate` | also make this provider and model current |
| `--no-verify` | skip the one-token check that the key and endpoint work |
| `--yes` | write without showing the preview |

```toml
# Provider in use; every vendor is declared under [providers.<slug>].
active = "deepseek"

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

[providers.deepseek]
model = "deepseek-v4-flash"
base_url = "https://api.deepseek.com"
api_key = "sk-xxx"
reasoning_effort = "high"

[providers.xai]
api_key = "xai-xxx"
reasoning_effort = "low"

# Only for a model oven does not ship: the wire id is the table key, and the
# limits are what turn on ctx% display and auto-compaction. Any of those keys
# can instead sit in the provider table itself, as a default for every model
# below it that leaves the field unset.
# [providers.myproxy]
# base_url = "https://example.com/v1"
# api_key = "sk-xxx"
# model = "my-model"
# context_window = 200000
# [providers.myproxy.models."my-model"]
# max_output_tokens = 8192
```

- `tools` — capabilities the agent can invoke (`file_read`, `file_write`,
  `bash`, `glob`, `grep`, `web_fetch`); an empty list means the defaults.
  `web_fetch` returns a page to the model: HTML becomes markdown unless
  `format` is `text`.
- `max_iters` — provider round trips one turn may take before the loop asks
  whether to continue; `[subagents]` tunes delegation (`enabled`,
  `max_concurrent`, and the subagents' own `max_iters`).
- `active` — the canonical vendor slug in use; aliases are accepted on read
  (`grok` → `xai`, `kimi` → `moonshot`, `glm` → `zhipu`). A known vendor needs
  only `api_key`: its endpoint, wire protocol and model catalog come from
  `oven-llm`. A custom vendor also needs `base_url`, and may set `protocol`
  (`completions` or `responses`) to override the default.
- a provider table may also carry `context_window`, `max_output_tokens`,
  `supports_system_prompt`, `supports_tools`, `supports_streaming` or
  `supports_vision` directly: they are the defaults every entry under
  `[providers.<slug>.models]` inherits wherever it leaves a field unset, so a
  vendor whose models share one window declares it once. A model that sets a
  field keeps its own value, and the stored entries stay as written.
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
| `/memory` | List, show or remove memories: `/memory [show <ref> \| rm <ref>]`, where `<ref>` is `workspace/<id>`, `user/<id>` or a bare id |

## Logs

Oven writes a rotating log to `~/.oven/logs/oven.log` (10 MiB per file, one backup). The TUI never prints logs to the terminal. Increase verbosity with `OVEN_LOG=debug` (or `RUST_LOG`).

## Build from source

```bash
cargo build -r
./target/release/oven
```

## License

[MIT](LICENSE).

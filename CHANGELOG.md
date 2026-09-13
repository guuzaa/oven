# Changelog

## [0.0.7] - 2026-09-14

### Added
- Ask mode (`Shift+Tab` cycles Agent → Plan → Ask): read-only tools run freely, shell commands need interactive approval, write and MCP tools are hidden
- Rotating file logs at `~/.oven/logs/oven.log` (10 MiB × 2 files) and tracing across the agent and runtime
- Auto-collapse thinking and tool details when the next message starts (new details start expanded; session-seeded ones stay collapsed; manual expands stay pinned)
- Collapsible diff rows for `file_edit` / `file_write` with per-line added/removed styling

### Fixed
- Normalize `\r\n` and lone `\r` to `\n` when pasting into the composer
- Treat wide characters as atomic in transcript selection
- Pin the transcript to the bottom on user input and `!` shell commands
- Show a reason when a tool is rejected or unavailable

## [0.0.6] - 2026-09-06

### Added
- `/compact` slash command and auto-compaction when history nears the model's context window
- New `oven-host` crate for host system interaction: process execution, command-output decoding, path confinement, and directory walking
- Collapsible tool results, toggled like thinking blocks
- Thinking block hover with double-click collapse
- Tool bursts grouped by action with detail summaries
- Turn elapsed time shown after each turn (`Worked for Xs`); thought elapsed time
- Mouse scrolling across multiple lines in the textarea

### Changed
- Split `AppCommand` into `Prompt` and `Control`; slash state changes and `Rewind` queue behind a running turn
- Move the system prompt module into `oven-agent`
- Parse `TodoWrite` args with serde
- Unify transcript viewport anchoring and trim redundant TUI state

### Fixed
- Garbled shell output on Windows
- Refresh the file list when using `@` to mention files

## [0.0.5] - 2026-09-01

### Added
- `!` local shell in the composer (host bash/PowerShell in the workspace, no LLM turn)
- Diff rendering for `file_edit` / `file_write` in the transcript
- Persist vendors under `[providers.<slug>]` so `/setup` and `/model` reuse saved API keys

### Changed
- Merge `AppHandle` into `App`; construct via `AppBuilder`
- Status bar reports last-turn token usage; drop history budget trimming
- Extract `walk_dir` into `oven-agent`; `oven-app` no longer depends on `ignore`
- Reorganize prompt templates under `prompt_template/`
- Fold `turn.rs` into `runtime/mod.rs`
- Slash replies shown as a toast

### Fixed
- Keep the selected model visible when scrolling the picker
- Cursor blinking in the input
- `file_read` results include line numbers
- Flaky tests on Windows

## [0.0.4] - 2026-08-24

### Added
- @ syntax to mention files in chat

### Changed
- Move AppEvent to a single mod
- Simplify env detection in system prompt
- Enhance event protocol
- Move resolve_session into Session::resolve

### Fixed
- Fix UI custom models displaying text
- Put effort value right after model name with a space
- Add rounded input border and quieter motion to TUI

## [0.0.3] - 2026-08-20

### Changed
- Adopt oven-llm Router (0.4.1) for model/provider routing
- Replace provider `kind` with `protocol` (custom vendors only); rewrite aliases `grok`/`kimi`/`glm` to `xai`/`moonshot`/`zhipu`
- Stop sending a default `max_tokens` of 4096
- File tools use async I/O
- Restructure TUI widgets under `components/`

### Fixed
- Accumulate token usage of all provider responses in a turn

## [0.0.2] - 2026-08-18

### Added
- Tool system architecture and UI overhaul

### Changed
- Replaced XDG directories with a single ~/.oven home directory
- Split transcript.rs into modular components

### Fixed
- UI full paint when scrolling
- Enlarged truncate length for tool results


## [0.0.1] - 2026-08-16
- Initial Release
# Changelog

## [Unreleased]

### Fixed
- Restore the per-turn `Worked for Xs` transcript separator after answers, tool follow-ups, cancellations, failures, compaction, and app errors, including the turn duration when resuming a session from its persisted timestamps

## [0.0.8] - 2026-09-22

### Added
- Live token usage: the status bar and context readout update as each provider response arrives, instead of once per turn
- File edit/write results are collapsible in the transcript, toggled like thinking blocks
- Tool bursts are titled by count (e.g. `Ran 2 commands, Read 1 file, 1 failed`) with the individual call lines in a collapsible body
- Newest-only live bodies: thinking deltas and open tool bursts render only their most recent screen rows, marked with an "N earlier lines" gap, so streaming cannot scroll the view upward
- `@` file mention now lists directories too, and `Tab` completes a directory without submitting

### Changed
- Assistant prose renders a single bullet gutter on its first line, with continuation lines indented under the body; other line kinds share the message indent
- Live bodies are capped by wrapped screen rows rather than logical lines
- Session files are appended from the already-persisted message count instead of rewriting every record after a turn
- History messages are shared as `Arc`, so publishing state and re-seeding the transcript cost refcounts instead of copying the conversation
- The agent owns the thinking clock and emits `StreamEvent::ThinkingDone { duration_ms }`, so the reported span, the transcript row, and the persisted session always agree; the non-streaming fallback now times its reasoning too
- Drop the per-turn `Worked for Xs` transcript separator and other dead code
- Bump `oven-llm` to 0.4.2 (new `raw_arguments` field on tool-use blocks)
- Install script prefers the `linux-gnu` artifacts (glibc ≥ 2.28) and falls back to musl, and fails fast when neither curl nor wget is available
- Add `docs/oven-tui.md` and `docs/oven-app.md`; refresh `docs/architectures.md`

### Fixed
- Close the thinking window before the answer or tool it led to
- Pin the clicked header row while a collapsed detail expands
- Keep transcript text selection alive across streaming events
- Align non-user rows with the user gutter
- Mid-turn `/model` switches publish the switching model's context window

## [0.0.7] - 2026-09-16

### Added
- Ask mode (`Shift+Tab` cycles Agent → Plan → Ask): read-only tools run freely, shell commands need interactive approval, write and MCP tools are hidden
- Rotating file logs at `~/.oven/logs/oven.log` (10 MiB × 2 files) and tracing across the agent and runtime
- Auto-collapse thinking and tool details when the next message starts (new details start expanded; session-seeded ones stay collapsed; manual expands stay pinned)
- Collapsible diff rows for `file_edit` / `file_write` with per-line added/removed styling
- Choice popup when the agent loop reaches its iteration limit, letting the user continue or end the turn
- `--version` prints the commit hash and release date

### Fixed
- Unknown slash commands pass through to the model instead of erroring
- Normalize `\r\n` and lone `\r` to `\n` when pasting into the composer
- Treat wide characters as atomic in transcript selection
- Pin the transcript to the bottom on user input and `!` shell commands
- Show a reason when a tool is rejected or unavailable
- Dismiss a finished todo list when the next message is sent
- Include the last 8 characters of the session id in log entries

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
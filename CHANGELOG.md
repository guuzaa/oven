# Changelog

## [Unreleased]

### Added
- A provider table can carry `context_window`, `max_output_tokens` and the `supports_*` flags directly, as defaults every `[providers.<slug>.models."<wire-id>"]` entry inherits wherever it leaves a field unset. A model that sets a field keeps its own value, an entry that only inherits is dropped when the file is rewritten, and the stored entries stay as written, so saving never bakes an inherited value into a model that only meant to follow the provider

### Changed
- **Breaking:** `[provider]` is gone: the active vendor is the root `active = "<slug>"`, and a provider is named by its `[providers.<slug>]` table key instead of a repeated `name`. Model metadata moves back to keyed tables — `[providers.<slug>.models."<wire-id>"]` — keeping the id out of the body, so ids with dots (`gpt-4.1`) just need quoting. Files still in the old shape keep loading and are rewritten on the next save, and a declared model now merges field by field across the user and project files instead of the newer entry replacing the older one outright
- `ProviderConfig::suggested_base_url` reads `oven_llm::ProviderName::base_url` instead of keeping its own copy of the same five endpoints

## [0.0.9] - 2026-09-30

### Added
- Subagents: the `task` tool delegates a self-contained job to another agent (`explore` — read-only tools — or `general`) with its own context, and `task_output` reports on one, or on all of them
- Subagent lifecycles: a registry with a concurrency cap (further spawns wait as `queued`), `role#n` addressing, and cancellation by name, by the turn that spawned them, or by app shutdown
- `/agents` lists subagents and stops, drops or focuses one (`x` stops the focused one); the TUI shows them in a strip above the composer (active first, `+N` when capped) and gives each its own transcript, opened with `/agents <n>` or by clicking its row
- `answer` tool: the model asks a question mid-turn and blocks until the answer arrives, which then becomes the tool result, with the question, option labels and descriptions clamped to their display limits. The question renders as an overlay prompt listing the proposed answers plus an `Other…` row that hands the composer the keystrokes, so any answer can be typed; `Esc` skips it and `Ctrl-C` cancels the turn
- A pending request is mirrored as `ToolEvent::QuestionAsked` with `AppPhase::Awaiting` and resumed by `App::respond`, and `AppEventKind::RequestResolved` closes its overlay even when the turn ends still holding it
- `max_iters` is configurable and bounds one run rather than one agent; `[subagents]` configures `enabled`, `max_concurrent` and the subagents' own `max_iters`
- `TurnEvent::StepStarted` / `StepFinished` bound each provider round trip, and `Agent::step` is public and returns a `Step` (text, calls, usage), so loop progress is observable and the loop is one policy over a step rather than the only way to drive an agent
- Tools receive the run's `TurnContext`, which carries cancellation, mode, model and the channel an interactive tool asks its question on
- Per-model metadata declared as `[[providers.<slug>.models]]` accepts `max_output_tokens` and the `supports_system_prompt` / `supports_tools` / `supports_streaming` / `supports_vision` flags next to `context_window`: omitted limits stay unknown (validation skips them) and omitted capabilities count as supported, so declaring a model never silently disables it
- A tool call in flight shimmers its transcript row the way thinking does, until its result lands: the burst row while a grouped call has yet to answer, and each call rendered as its own row

### Changed
- **Breaking:** `AppCommand` and `ControlCommand` are gone. `App::submit(text)` classifies the text once into an `Input` (`Chat`, `Shell`, `Slash`, `Rewind`) and returns it, so the runtime and the TUI no longer sniff the same string for the same syntax
- **Breaking:** what needs no conversation driver is a method on `App`, applied by the caller instead of queued behind the turn it affects: `cancel(turn_id)`, `set_mode`, `respond`, `stop_subagent`, `stop_subagents`. `/model`, `/agents` and `/exit` still apply mid-turn, and `shutdown` cancels the running turn directly
- **Breaking:** `TurnContext::new` takes the agent's `Selection` (its mode and model) instead of copies, `set_mode` / `set_model` are gone, and a turn asks the user through a `RequestSink` (`TurnContext::with_requests`) that `Shared` stores, announces and answers by id, so nothing is copied back or relayed when a run ends
- **Breaking:** `StateChange` and `StateEvent` are gone: mode, model, context window, providers, models, subagents, todos and usage are read from `AppState`, which the TUI follows through the `watch` channel. The one event left is `AppEventKind::HistoryChanged { reason }`
- **Breaking:** model metadata moves from the quoted `[providers.<slug>.models."<wire-id>"]` table keys to `[[providers.<slug>.models]]` entries with an `id` field, so ids containing dots (`gpt-4.1`, `glm-5.3`) need no quoting; entries merge by `id` and are rewritten sorted when the config is saved
- `AppState` lives only in its `watch::Sender` and is changed in place, queued prompts are counted in the inbox so `App::shutdown` reports every prompt the driver never took, and tools mount as `Arc<dyn Tool>` shared by subagents
- `AppPhase::Compacting` marks the driver busy summarizing and the TUI is busy exactly while `phase.is_active()`, and `AppPhase::Awaiting` carries a pending request
- `/setup` rebuilds the router from config and swaps the snapshot in (`Agent::replace_router`) instead of mutating a live router in place, which used to be able to panic while another agent was mid-request
- Only the driver's own events move app state, and every agent reports on one event channel the runtime drains while idle, so a subagent's progress and usage no longer land in the main turn's readout and reach the frontend between turns
- The turn driver moves to `runtime/turn.rs`: the runtime actor keeps the command loop and persistence, and what happens while a turn runs sits beside it
- `oven-app`, `oven-agent` and `oven-tui` are layered (`core` / `runtime` / `capabilities` / `api` / `platform` / `widgets`) with each `lib.rs` a map of those layers, and MCP servers are declared in config instead of the MCP layer
- `/model` and `/setup` completion lists filter by the characters typed: the query is rendered in the composer (`/model deep`), `Tab` completes the highlighted entry into it, and the provider and protocol stages of the wizard narrow the same way; `oven_app::complete` owns the prefix rule shared by the slash popup, the model picker and the wizard
- Transcript chrome: user prompts and `!` shell commands are framed like the composer, with the submitted prompt as the first row of its turn, and the hint row moved to the composer's bottom-right
- `docs/subagents.md`, `docs/architectures.md`, `docs/oven-app.md` and `docs/oven-tui.md` cover the subagent design, the question channel, the new phase and the crate layers
- `Tool::result_detail` lets a tool name the body its row ends on when it lands, so `ToolEvent::Finished` carries it beside the result; a question row shows the answer the user picked instead of repeating the choices the prompt already offered
- A turn's questions are counted in the burst title the way its searches, reads and commands are: `Asked 1 question`, `Asked 2 questions`
- An answer opens with its question: what a call landed on is pinned, so a burst reopened after the turn moved on lists each question with the answer it got instead of hiding the answers behind a second click

### Fixed
- The composer no longer garbles itself: a deleted wide glyph leaves no stale half-cell, the prompt stays `>`, the hardware cursor is parked on the caret for IME, and the prompt stays pinned until the viewport leaves its turn
- Tool calls from one response run at the same time instead of one after another, so asking for several `task`s actually runs several subagents in parallel; approvals are still asked one at a time, tools that declare `ToolCaps::exclusive` (file edits and writes, the todo list, the question tool) take a turn each, and tool results still enter the history in the order the model asked for them
- `/agents` applies while a turn is running and opening a subagent builds its view on demand, so it can be watched from the moment it is spawned instead of only after the reply lands; its clock stops when it does, so a cancelled or failed subagent shows the time it took instead of counting up forever
- `Ctrl-C` stops what is still running before quitting — the turn, then the subagents it spawned — and reports the messages that were queued but never sent on the way out
- Leaving a subagent's transcript takes a single `Esc` instead of the usual confirmation pair, so `Esc` first closes an open viewer before it cancels a turn or rewinds; the hint row spells the way back out, the arrow keys scroll it, and `Ctrl-C` still quits from inside the viewer
- Restore the per-turn `Worked for Xs` transcript separator after answers, tool follow-ups, cancellations, failures, compaction, and app errors, including the turn duration when resuming a session from its persisted timestamps
- A verbose question no longer fails the `answer` call, which used to leave the user with a failed tool row and no prompt at all, and the question prompt grows with the question's wrapped text instead of clipping it to a fixed block
- Composer slash and mention completions stay shut while a question waits for a typed answer, so `Tab` can no longer complete command text into the answer
- Declaring a model no longer registers it with every capability off, which made oven reject each turn locally with `provider: invalid request` before the request ever reached the upstream API
- Provider request-validation failures name the rule that rejected them (`max_tokens 4096 exceeds model's max_output_tokens 8192`, `model does not support tools`, …) instead of only the bare `invalid request`
- Expanding or collapsing a transcript block no longer yanks the view to that block's line: the anchor is the first body line the layout would draw, which accounts for the pinned prompt, so the clicked header keeps its screen row and the body grows below it
- Tool rows share the thinking color, matching the in-flight shimmer
- The install scripts skip the download when no tag is pinned and the requested version is already installed

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
# Changelog

## [Unreleased]

### Added
- Subagents: `task` delegates a self-contained job to another agent with its own context, as `explore` (read-only tools) or `general` (everything but the tools that speak to the user or would nest delegation); `task_output` reports on one, or on all of them
- Subagent lifecycles: a registry with a concurrency cap (further spawns queue as `queued`), `role#n` addressing, and cancellation by name, by the turn that spawned them, or by app shutdown
- `/agents` lists subagents and stops, drops or focuses one; `x` stops the focused one
- The TUI shows subagents in a strip above the composer (active first, `+N` when capped) and gives each its own transcript, opened with `/agents <n>` or by clicking its row
- `Esc` closes an open subagent viewer before it cancels a turn or rewinds
- `max_iters` is configurable (`max_iters`, default 200) and bounds one run rather than one agent; `[subagents]` configures `enabled`, `max_concurrent` and the subagents' own `max_iters`
- `TurnEvent::StepStarted` / `StepFinished` bound each provider round trip, so loop progress and its stop reason are observable from the event stream
- `Agent::step` is public and returns a `Step` (text, calls, usage), making the loop one policy over it rather than the only way to drive an agent
- Tools receive the run's `TurnContext`, which carries cancellation, mode, model and the channel an interactive tool asks its question on
- A tool call in flight shimmers its transcript row the way thinking does, until its result lands: the burst row while a grouped call has yet to answer, and each call rendered as its own row

### Changed
- **Breaking:** `AppCommand` and `ControlCommand` are gone. `App::submit(text)` classifies the text once into an `Input` (`Chat`, `Shell`, `Slash`, `Rewind`) and returns it, so the runtime and the TUI no longer sniff the same string for the same syntax
- **Breaking:** what needs no conversation driver is a method on `App`, applied by the caller instead of queued behind the turn it affects: `cancel(turn_id)`, `set_mode`, `respond`, `stop_subagent`, `stop_subagents`. `/model`, `/agents` and `/exit` still apply mid-turn, and `shutdown` cancels the running turn directly
- **Breaking:** `TurnContext::new` takes the agent's `Selection` (its mode and model) instead of copies, and `TurnContext::set_mode` / `set_model` are gone. The agent and the turn share one value, so the agent no longer copies it back when a run ends
- **Breaking:** a turn asks the user through a `RequestSink` (`TurnContext::with_requests`) instead of a channel the runtime drains. The app's `Shared` stores the pending request, announces it and answers it by id, so the runtime no longer relays it
- **Breaking:** `StateChange` and `StateEvent` are gone: mode, model, context window, providers, models, subagents, todos and usage are read from `AppState`, which the TUI follows through the `watch` channel. The one event left is `AppEventKind::HistoryChanged { reason }`, for a history replaced wholesale
- `AppPhase::Compacting` marks the driver busy summarizing, and the TUI is busy exactly while `phase.is_active()`, so a slash command that leaves the phase alone no longer needs a notification to clear it
- `AppState` lives only in its `watch::Sender` and is changed in place, instead of a copy the runtime cloned and published after each change
- Queued prompts are counted in the inbox rather than in the runtime's deferral queue, so `App::shutdown` reports every prompt the driver never took
- Tools mount as `Arc<dyn Tool>` and subagents share the driver's instances rather than rebuilding them
- `/setup` rebuilds the router from config and swaps the snapshot in (`Agent::replace_router`) instead of mutating a live router in place, which used to be able to panic while another agent was mid-request
- Only the driver's own events move app state: a subagent's usage no longer lands in the context readout, and `App::prompt` no longer returns early on a subagent's turn completion
- Every agent reports on one event channel the runtime drains while idle, so subagent progress reaches the frontend between turns
- The turn driver moves to `runtime/turn.rs`: the runtime actor keeps the command loop and persistence, and what happens while a turn runs sits beside it
- `docs/subagents.md` records the subagent design: the decisions behind it, the constraints it removed, and the seams it leaves for loop and graph engineering
- `/model` and `/setup` completion lists filter by the characters typed: the query is rendered in the composer (`/model deep`), `Tab` completes the highlighted entry into it, and the provider and protocol stages of the setup wizard now narrow the same way
- `oven_app::complete` owns the prefix rule shared by the slash popup, the model picker and the setup wizard
- `answer` tool: the model can ask the user a question mid-turn and blocks until they reply, and the reply becomes the tool result. The question renders as an overlay prompt listing the proposed answers plus an `Other…` row that hands the composer the keystrokes, so any answer can be typed; `Esc` skips the question and `Ctrl-C` cancels the turn
- A pending question is mirrored as `ToolEvent::QuestionAsked` and `AppPhase::Awaiting`, resumed by `App::respond`
- `AppEventKind::RequestResolved` reports that a pending user request closed, so an overlay prompt closes for that request id when it is answered or dropped, including when the turn ends still holding it
- Per-model metadata declared as `[[providers.<slug>.models]]` accepts `max_output_tokens` and the `supports_system_prompt` / `supports_tools` / `supports_streaming` / `supports_vision` flags next to `context_window`, with the wire id in an `id` field instead of a quoted table key: omitted limits stay unknown (validation skips them) and omitted capabilities count as supported, so declaring a model never silently disables it
- The model picker no longer prints its filter inside the popup, since the composer line shows it
- The question prompt grows with the question's wrapped text instead of clipping it to a fixed block
- The `answer` tool clamps its question, option labels and descriptions to their display limits, and clamps the answer before it enters the conversation
- `docs/architectures.md`, `docs/oven-app.md` and `docs/oven-tui.md` cover the question channel, the new phase and the prompt
- Model metadata moved from the quoted `[providers.<slug>.models."<wire-id>"]` table keys to `[[providers.<slug>.models]]` entries with an `id` field, so ids containing dots (`gpt-4.1`, `glm-5.3`) need no quoting; entries merge by `id` and are rewritten sorted when the config is saved

### Fixed
- Deleting wide characters in the composer no longer leaves white blocks: the trailing column of a wide glyph is blank in both frames, so the diff never cleared it
- Tool calls from one response run at the same time instead of one after another, so asking for several `task`s actually runs several subagents in parallel; approvals are still asked one at a time, tools that declare `ToolCaps::exclusive` (file edits and writes, the todo list, the question tool) take a turn each, and tool results still enter the history in the order the model asked for them
- `/agents` applies while a turn is running, and opening a subagent builds its view on demand, so a subagent can be watched from the moment it is spawned instead of only after the reply lands
- A subagent's clock stops when it does, so a cancelled or failed subagent shows the time it took instead of counting up forever
- `Ctrl-C` stops what is still running before quitting — the turn, then the subagents it spawned — and reports the messages that were queued but never sent on the way out
- Leaving a subagent's transcript takes a single `Esc` instead of the usual confirmation pair, the hint row spells the way back out, and the arrow keys scroll it; `Ctrl-C` still quits from inside the viewer
- Restore the per-turn `Worked for Xs` transcript separator after answers, tool follow-ups, cancellations, failures, compaction, and app errors, including the turn duration when resuming a session from its persisted timestamps
- A verbose question no longer fails the `answer` call, which used to leave the user with a failed tool row and no prompt at all
- Composer slash and mention completions stay shut while a question waits for a typed answer, so `Tab` can no longer complete command text into the answer
- Declaring a model under `[[providers.<slug>.models]]` no longer registers it with every capability off, which made oven reject each turn locally with `provider: invalid request` before the request ever reached the upstream API
- Provider request-validation failures name the rule that rejected them (`max_tokens 4096 exceeds model's max_output_tokens 8192`, `model does not support tools`, …) instead of only the bare `invalid request`

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
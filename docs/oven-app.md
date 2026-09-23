# oven-app

`oven-app` is the application layer. It composes `oven-agent` (the agent loop
and tools), `oven-host` (shell and process infrastructure) and `oven-llm`
(providers, routing, model catalog) into one long-lived task driven by
commands, and publishes facts back as events. It makes no rendering decisions
and owns no presentation state: `oven-tui` is a client of this crate, so the
same runtime also serves headless runs.

Everything a frontend can reach goes through two channels:

```
App::send(AppCommand)  ──▶  Runtime::run  ──▶  AppEvent  ──▶  App::subscribe()
                              │
                              ├─ Agent (turns, tools, todos, history)
                              ├─ shell (host process)
                              ├─ SlashRegistry (/model, /setup, …)
                              ├─ SessionStore (JSONL on disk)
                              └─ watch::Sender<AppState>  (snapshot state)
```

## Entry chain

| File | Role |
| --- | --- |
| `src/lib.rs` | re-exports the public surface. |
| `src/app.rs` | the `App` handle: command sender, event subscriptions, state accessors. |
| `src/builder.rs` | service composition: config → tools, MCP servers, skills, agent. |
| `src/runtime/mod.rs` | the runtime task itself — command loop, turn execution, persistence. |

`App` has three constructors:

```text
App::builder(root) ─▶ load_config() ─▶ open()            // no session on disk
                                 └──▶ open_session(id)   // JSONL under ~/.oven/sessions
App::open(root)     ─▶ builder + open; a broken config is an error
App::query(root, p) ─▶ open, run one prompt, shut down; used for headless runs
```

Only `open_session` reaches an LLM (it builds the interactive router); `open`
uses the non-interactive one. `open_session(None)` resolves the newest session
for the canonicalized root through the `cwd_latest.json` index, or starts a fresh
one with a uuid v7 id the caller never supplies. `AppBuilder::with_config`
bypasses the filesystem entirely, which is how the tests build an app.

`App::prompt` is the convenience path used by both `App::query` and the tests: it
subscribes, sends `AppCommand::Prompt`, then collects text deltas until the turn
completes, fails, cancels, a shell command finishes, or a non-turn notification
arrives. Loop-limit prompts are answered with `LoopLimitDecision::Exit` so a
headless run always terminates.

## Commands and events

`command.rs` keeps prompts and control instructions structurally distinct, so no
caller ever has to guess whether a piece of text is chat or a command — the
classification happens once, inside the runtime, where the slash registry lives:

```rust
pub enum AppCommand {
    Prompt(String),
    Control(ControlCommand),   // Cancel, SetMode, RespondToolApproval, RespondLoopLimit, Rewind
    Shutdown,
}
```

Anything that needs the agent's exclusive borrow (switching models, clearing
history) is therefore expressed as slash text through `Prompt` and resolved once
the agent is free. `ControlCommand` covers exactly what can be applied while a
turn holds the borrow.

`event.rs` fans events out to every subscriber on its own `UnboundedSender`,
auto-pruning dead ones, with a monotonic `seq`:

| `AppEventKind` | Emitted when |
| --- | --- |
| `Agent(AgentEventEnvelope)` | streamed text, thinking, tool calls and results, turn start/end |
| `StateChanged(StateEvent)` | a coarse-grained state delta (see [State](#state)) |
| `Shell(ShellEvent)` | `!cmd` started, finished, failed |
| `Compaction(CompactionEvent)` | history compaction started, completed (before/after tokens), failed |
| `Notification { text }` | one-shot backend replies: `/model` confirmations, errors that are not fatal |
| `Error { message }` | turn or IO failure; the runtime keeps running |
| `Exited` | `/exit` accepted |

## Runtime loop

`Runtime` owns the agent plus everything the agent does not: the router handle,
the session store, the config, the event bus and the shared state. `run` pops
from the deferred queue first, then blocks on the command channel; every command
is logged by kind. `AppCommand::Prompt` is the only branch that does real work,
and it classifies the input once:

| Input | Path |
| --- | --- |
| `!command` | `run_shell` — a host process, not a turn |
| `/name args` | `SlashRegistry::parse_and_run`; `Passthrough` falls through to a turn |
| anything else | `agent.run(input, ctx, sink)` |

A turn is driven by a `tokio::select!` with `biased` priority, so cancellation
and approvals are never starved by a flood of stream deltas:

| Branch | Purpose |
| --- | --- |
| `cmd_rx.recv()` | shutdown, cancel, tool-approval and loop-limit replies, mid-turn `/model`; anything else is deferred |
| `approval_rx.recv()` | agent asked for a tool approval → phase becomes `AwaitingToolApproval` |
| `loop_limit_rx.recv()` | agent hit the iteration cap → phase becomes `AwaitingLoopLimit` |
| `agent_rx.recv()` | one agent event, forwarded and mirrored into `AppState` |
| `turn` | the turn future itself |

`/model` is the one slash command that runs mid-turn: validating it needs only a
`Router` snapshot and applying it only a `TurnContext`, so it never contends for
the `&mut Agent` the turn holds. Other slash commands and rewinds are pushed onto
`pending` with a `queued: will apply once the current reply finishes` notice.

After the turn: session meta is stamped, trailing agent events are drained,
the turn is persisted, state is synced, and if context usage reached
`compact_threshold` the history is compacted. The phase returns to `Idle`
regardless, so a failed turn never wedges the UI.

## State

`state.rs` mirrors the agent into a cloneable `AppState` published on a
`watch` channel, so a frontend renders progress by diffing one small struct
instead of buffering the whole history itself:

```rust
pub enum AppPhase {
    Idle,
    Running { turn_id },
    AwaitingToolApproval { turn_id, request: PendingToolApproval },
    AwaitingLoopLimit { turn_id, request_id, max_iters },
    Cancelling { turn_id },
    ShuttingDown,
}
```

Alongside the snapshot, coarse-grained deltas go out as `StateChange`:
`ModelChanged`, `ModeChanged`, `TodosChanged`, `HistoryChanged`,
`SessionChanged`, `UsageChanged`, `ContextChanged`, `ProviderChanged`,
`ModelsChanged`. The phase carries the approval payload, so the approve/reject
modal can be rendered without a second query. `HistoryChanged` names its
`HistoryChangeReason` (`Rewound`, `Cleared`, `Compacted`, `External`), so a view
tells a rewind from a `/clear` without inferring it from the revision number.

`context_tokens` is prompt-side tokens (input plus cache reads) of the last
response and `context_window` comes from the router's model info; both refresh on
every `AgentEvent::Usage`, so the ctx% moves during a turn instead of only at
its end. Unknown windows disable the ctx% display and auto-compaction.

## Sessions

`session.rs` persists a conversation as JSONL: one `Record` per line, either a
`Message`, a `TokenUsage` (written once after the final assistant message of a
user turn), a `Thinking` span for the reasoning that preceded an assistant
message (start timestamp plus `duration_ms`), a `TodoList` snapshot or
`SessionMeta`, each with a Unix-ms timestamp, appended as the turn progresses so
a crash loses nothing beyond the last unflushed line. Reading accepts older
formats — bare messages and the `{message, usage}` envelope — with timestamp 0.

Persistence is incremental: `persist_turn` appends only the records past
`persisted_messages`, so a turn costs one open/flush cycle no matter how long the
conversation is. A structural reset (`clear`, restoring a session) bumps the
history revision, which drops `persisted_messages` back to 0 and re-syncs
`persisted_rev`, so the next write re-syncs from the start of history instead of
skipping the reset. `rewind` rewrites the file outright (`overwrite` truncates
it), while `/compact` and `/clear` start a *new* session file holding only the
compacted history or a fresh todo snapshot; the recent-session index is updated
so `oven --continue` lands on it next time.

`SessionStore` is shared behind a `Mutex` so the runtime can point it at a new
file mid-session, and reports the session id only once the file has content —
a session that was opened but never persisted stays invisible to `/continue`.

## Slash commands

`slash/` defines one trait and six built-ins; the registry is extensible:

```rust
pub trait SlashCommand: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn execute(&self, agent: &mut Agent, args: &str) -> Result<CommandOutcome, AppError>;
}
```

| Command | Does |
| --- | --- |
| `/model [id] [effort]` | switches (or reports) the model and reasoning effort; writes the overlay to the user config |
| `/setup name=… api_key=… model=… base_url=…` | merges a provider overlay, rebuilds the client, upserts it into the router |
| `/compact` | summarizes history into a fresh session file |
| `/clear` | clears history, todos and session |
| `/exit` | emits `goodbye` and `Exited` |
| `/plan on/off` | toggles plan mode, replying with the current mode and the mode list |

Commands return a `CommandOutcome` the runtime interprets — `Reply` becomes a
notification, `Passthrough` falls through to an agent turn, `Exit` emits
`Exited`, `ModeChanged` switches the live mode, `ModelChanged`/
`ProviderChanged` rebuild routing, and `Cleared` and `Compact` touch
persistence. Nothing in `slash/` performs IO of its own.

## Providers and config

`provider.rs` turns config into a `Router`. Every provider with a usable key is
registered wrapped in `RetryingProvider` (timeout, retries, exponential backoff
from config). If nothing registers, `open` fails with
`no API key for any provider; set provider.api_key or run /setup`, but the
interactive path deliberately returns an *empty* router instead, so the startup
can show the setup wizard rather than an error.

`config.rs` merges a user-level `~/.oven/config.toml` (created from
`config.example.toml` on first run) with a project-level `.oven.toml`, project
winning, with env vars layered last (`OVEN_MODEL`, `OVEN_BASE_URL`, `OVEN_API_KEY`).
Known vendor names are canonicalized (`grok` → `xai`, `kimi` → `moonshot`), unknown
vendors require a `base_url`, and per-model context windows can be declared so
custom gateways still get ctx% and auto-compaction. `save_provider_at` rewrites
the file in the canonical format whenever `/model` or `/setup` changes something.

Switching providers at runtime (`set_provider`) fills missing fields from the
saved entry and the vendor presets, validates that a key is present, and only
then swaps the client into the live router — a bad key never destroys the
working one. After the swap the model list is refreshed from `list_models()`
with a short timeout; an auth error surfaces as `API key rejected: …`.

## Tools, MCP and skills

`tools.rs` mounts a named set of tools per workspace: `file_read`,
`file_write`, `file_edit`, `bash`, `glob`, `grep`, plus `todo_write` and
`read_skill` added by the builder. An empty config list means the built-in
defaults; unknown names are skipped silently.

`mcp/` declares MCP servers in config (`mcps.<id>`, stdio via `command`/`args`/
`env`, or streamable HTTP via `url`/`headers`) and connects them at agent build
time. Each server's `tools/list` is bridged into the agent as
`<server_id>_<tool_name>`, so the model calls them like any other tool. The
protocol sits behind two small traits — `McpCaller` (one `tools/call`) and
`McpConnector` (connect and list) — which lets tests mock the server side with
mockall instead of spawning a process.

Skills are deliberately *not* tools: they contribute system-prompt guidance and
are read through `read_skill`, discovered from `~/.oven/skills` then the
project's `.oven/skills` (later paths override).

## Shell mode

An input starting with `!` is executed as a host command in the workspace root
with a 300 s timeout — no LLM turn is involved. `run_shell` reuses the same
select loop as a turn, so `ControlCommand::Cancel` stops the process through a
`CancellationToken`. `shell.rs` then formats the result into a `<local-shell>`
envelope (command, exit code, stderr section, output) and pushes it into the
history as a user message, so the model sees what the user just ran. The same
envelope is what `LocalShell::try_parse` understands when it arrives back from
the model.

`mention.rs` is a separate utility for the frontend: a `nucleo`-backed fuzzy
search over the workspace files with background rescans, so `@` completion never
blocks the UI thread. It never touches the agent.

## Tests

Unit tests live in-file (`session.rs`, `slash/*`, `mcp/client_test.rs`); the
runtime's behaviour — command ordering, deferred commands, mid-turn model
switches, compaction, rewinds, session switching, approval and loop-limit
handling — is covered by `runtime/runtime_test.rs` with a mocked provider.
`tests/` holds the integration paths: config merging, MCP wiring through a mock
connector, and tool mounting. Nothing waits on wall-clock sleeps, and timeouts
are always explicit.

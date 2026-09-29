# oven-app

`oven-app` is the application layer. It composes `oven-agent` (the agent loop
and tools), `oven-host` (shell and process infrastructure) and `oven-llm`
(providers, routing, model catalog) into one long-lived task driven by
commands, and publishes facts back as events. It makes no rendering decisions
and owns no presentation state: `oven-tui` is a client of this crate, so the
same runtime also serves headless runs.

A frontend reaches the app through one handle, `App`, in three ways:

```
App::submit(text)  ──▶ inbox ──▶ Runtime::run ──▶ AppEvent ──▶ App::subscribe()
                                   │
App::cancel / set_mode /           ├─ Agent (turns, tools, todos, history)
    respond / stop_subagent(s) ──▶ ├─ shell (host process)
    (applied by the caller,        ├─ SlashRegistry (/model, /setup, …)
     through `Shared`)             ├─ SessionStore (JSONL on disk)
                                   └─ Shared.state: watch::Sender<AppState>
```

What has to wait for the conversation driver goes through the inbox, one input
at a time. What does not — cancelling, switching mode, answering a request,
stopping subagents, `/model`, `/agents`, shutdown — is applied by the caller
on `Shared`, so it never queues behind the turn it wants to affect.

## Entry chain

| File | Role |
| --- | --- |
| `src/lib.rs` | re-exports the public surface. |
| `src/app.rs` | the `App` handle: `submit`, control methods, event subscriptions, state accessors. |
| `src/command.rs` | `Input`: what the user submitted, classified once at the boundary. |
| `src/inbox.rs` | the queue of inputs for the driver, counting the prompts nobody took. |
| `src/shared.rs` | what `App` and the runtime both reach: state, events, the running turn, its pending request, shutdown. |
| `src/shared/live.rs` | what applies while a turn holds the driver: `/model`, `/agents`, `/exit`. |
| `src/builder.rs` | service composition: config → tools, MCP servers, skills, agent. |
| `src/runtime/mod.rs` | the runtime actor — input loop, dispatch, persistence. |
| `src/runtime/turn.rs` | what happens while a turn runs — the driver turn, shell, slash commands. |
| `src/subagent.rs` | delegated runs — the registry, the concurrency cap, one task per subagent. |

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

`App::shutdown` cancels the running turn and returns how many queued prompts
the runtime dropped without ever running them, so a frontend can tell the user
what it lost on the way out.

`App::prompt` is the convenience path used by both `App::query` and the tests: it
subscribes, submits the prompt, then collects text deltas until the turn
completes, fails, cancels, a shell command finishes, or a non-turn notification
arrives. Loop-limit prompts are answered with `LoopLimitDecision::Exit` so a
headless run always terminates.

## Inputs and events

`App::submit(text)` classifies the text once, with the slash registry the
runtime shares, into an `Input`, and returns it so a frontend can draw it the
way the runtime will treat it:

```rust
pub enum Input {
    Chat(String),
    Shell(String),                          // `!command`; empty is kept so the runtime can say why nothing ran
    Slash { name: String, args: String },   // a registered command; unknown `/x` stays `Chat`
    Rewind,
}
```

While a turn runs, `submit` first tries to apply the input on the spot
(`Shared::apply_now`): `/model` and any command that never asks for the agent
(`/agents`, `/exit`). Anything else goes to the inbox, with a
`queued: will apply once the current reply finishes` notice for slash commands
and rewinds, and waits for the driver.

`event.rs` fans events out to every subscriber on its own `UnboundedSender`,
auto-pruning dead ones, with a monotonic `seq`:

| `AppEventKind` | Emitted when |
| --- | --- |
| `Agent(AgentEventEnvelope)` | streamed text, thinking, tool calls and results, turn start/end |
| `HistoryChanged { reason }` | the history was replaced wholesale — rewind, `/clear`, compaction (see [State](#state)) |
| `Shell(ShellEvent)` | `!cmd` started, finished, failed |
| `Compaction(CompactionEvent)` | history compaction started, completed (before/after tokens), failed |
| `Notification { text }` | one-shot backend replies: `/model` confirmations, errors that are not fatal |
| `Error { message }` | turn or IO failure; the runtime keeps running |
| `Exited` | `/exit` accepted |

## Runtime loop

`Runtime` owns the agent plus everything the agent does not: the session store
and the persistence bookkeeping. The state, event bus, config and router it
shares with `App` live in `Shared`. `run` selects, with `biased` priority, on
shutdown, the inbox and the subagent wake signal; every input is logged by
kind. It dispatches on the `Input`:

| Input | Path |
| --- | --- |
| `Shell` | `run_shell` — a host process, not a turn |
| `Slash` | `SlashRegistry::run`, then the outcome is applied |
| `Rewind` | truncate the last turn, persist, publish |
| `Chat` | `agent.run(input, ctx, sink)` |

A turn is driven by a `tokio::select!` with `biased` priority, so shutdown is
never starved by a flood of stream deltas:

| Branch | Purpose |
| --- | --- |
| `shutdown.cancelled()` | cancel the turn and take its result |
| `wake_rx.recv()` | the subagent registry changed → mirror it into `AppState` |
| `turn` | the turn future itself |

Everything else a user does mid-turn reaches the turn through `Shared`:

- `TurnContext` carries the agent's own `Selection` (mode and model), so
  `App::set_mode` and `/model` change it and the turn picks the change up at
  its next step.
- `TurnContext` carries `Shared` as its `RequestSink`. A tool approval, the
  loop-limit prompt or a question is stored as the turn's one pending request,
  announced as an agent event, and the phase becomes `Awaiting`. `App::respond`
  finds it by id and answers it directly; a reply of the wrong kind leaves it
  open.
- `App::cancel(turn_id)` cancels only if `turn_id` is still the running turn.

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
    Awaiting { turn_id },
    Cancelling { turn_id },
    ShuttingDown,
}
```

The snapshot is the only carrier of levels — mode, model, context window,
providers, models, subagents, the phase itself — so a frontend reads them as they
stand and no event repeats them. What an `Awaiting` turn waits for is the agent
event that announced the request, not part of the phase, and `Compacting` marks
the driver busy summarizing with no turn to cancel. The one thing a snapshot
cannot say is that the history was replaced, so that goes out as
`HistoryChanged { reason }` (`Rewound`, `Cleared`, `Compacted`, `External`): a
view rebuilds itself from the state and tells a rewind from a `/clear`.

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
pub struct CommandContext<'a> {
    pub agent: &'a mut Agent,
    pub subagents: &'a Subagents,
}

pub trait SlashCommand: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn execute(&self, cx: &mut CommandContext<'_>, args: &str) -> Result<CommandOutcome, AppError>;
}
```

A command reaches the driver and the subagent registry, and returns an outcome
the runtime applies. The registry is shared state a command may act on directly
— stopping or dropping a subagent is the registry's own business — while
anything that moves app state still comes back as an outcome.

`cx.agent()` hands back the driver, or `AppError::AgentBusy` while a running turn
holds it. That one call is what decides whether a command applies mid-turn or
waits: `/agents` never asks, so it works while a subagent is being watched;
`/clear`, `/setup`, `/plan`, `/model` and `/compact` all ask, so they queue.

| Command | Does |
| --- | --- |
| `/model [id] [effort]` | switches (or reports) the model and reasoning effort; writes the overlay to the user config |
| `/setup name=… api_key=… model=… base_url=…` | merges a provider overlay, rebuilds the client, upserts it into the router |
| `/compact` | summarizes history into a fresh session file |
| `/clear` | clears history, todos and session |
| `/exit` | emits `goodbye` and `Exited` |
| `/plan on/off` | toggles plan mode, replying with the current mode and the mode list |
| `/agents [stop <name\|all> \| forget <name>]` | lists subagents, or focuses, stops and drops one |

Commands return a `CommandOutcome` the runtime interprets — `Reply` becomes a
notification, `Passthrough` falls through to an agent turn, `Exit` emits
`Exited`, `ModeChanged` switches the live mode, `ModelChanged`/
`ProviderChanged` rebuild routing, and `Cleared` and `Compact` touch
persistence. Nothing in `slash/` performs IO of its own.

`complete.rs` holds the prefix rule the completion lists in the TUI select with:
ASCII-case-insensitive, an empty query keeping every key, and `matches_model`
also accepting the wire id a slug resolves to. `oven-tui` renders the indices
`select` returns, so the rule cannot drift between the slash popup, the picker
and the setup wizard.

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
`file_write`, `file_edit`, `bash`, `glob`, `grep`, `todo_write` and `answer`,
plus `read_skill` added by the builder. An empty config list means the built-in
defaults; unknown names are skipped silently.

`answer` is how the model asks the user something: it publishes a `Question`
(optionally with the answers to choose from) on the turn's question channel and
awaits the reply, which comes back as the tool's result. It needs a frontend, so
it fails with `no user is available to answer the question` on a bare
`Agent`.

## Subagents

`subagent.rs` supervises delegated work: one registry, one concurrency cap, one
tokio task per subagent. [`subagents.md`](./subagents.md) is the full design.

`AppBuilder` composes it with the driver, because the two have to share a
router and an event channel from the start:

```text
AppBuilder::build_agent_with_router
  ├── tools ──► Subagents::new(roles, router, settings)
  ├── main Agent::with_router(router, tools + task + task_output)
  └── AppAgents { main, subagents, events, event_rx, wake_rx }
```

`AppAgents` is what `spawn_runtime` takes. The channel is the reason it exists:
every agent — the driver and each subagent — reports there, so the runtime
drains it in its outer loop while it is idle, not only inside a turn. The
supervisor keeps the matching `wake` sender: a ping with no payload that says
the registry moved, so the runtime can read the snapshot and publish
`AppState.subagents` only when it actually differs.

The registry is the single source of truth. `AppState.subagents` mirrors it,
`/agents` reads it, and `task_output` reads it, so a panel and a tool can never
disagree. Subagents are never persisted: only the `task` call and its result
are in the driver's session, so a resumed session shows no subagents.

Config:

```toml
[subagents]
enabled = true        # mount task / task_output
max_concurrent = 4    # further spawns wait in `queued`
max_iters = 60        # provider round trips one subagent may take
```

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
select loop as a turn, so `App::cancel` stops the process through a
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
runtime's behaviour — input ordering, queued commands, mid-turn model
switches, compaction, rewinds, session switching, approval and loop-limit
handling — is covered by `runtime/runtime_test.rs` with a mocked provider.
`tests/` holds the integration paths: config merging, MCP wiring through a mock
connector, and tool mounting. Nothing waits on wall-clock sleeps, and timeouts
are always explicit.

# oven-app

`oven-app` is the application layer. It composes `oven-agent` (the agent loop
and tools), `oven-host` (shell and process infrastructure), `oven-mem` (durable
memory) and `oven-llm` (providers, routing, model catalog) into one
long-lived task driven by commands, and publishes facts back as events. It makes no rendering decisions
and owns no presentation state: `oven-tui` is a client of this crate, so the
same runtime also serves headless runs.

The crate is layered, and a layer reaches only into the ones below it:
`api` (the facade), `runtime`, `commands`, `capabilities`, `core` and
`platform`. One edge moves the other way: the runtime assembles the `App`
handle the facade hands out.

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
| `src/api/app.rs` | the `App` handle: `submit`, control methods, event subscriptions, state accessors. |
| `src/api/input.rs` | classifies submitted text into an `Input`. |
| `src/api/builder.rs` | service composition: config → tools, MCP servers, skills, memory, agent. |
| `src/core/input.rs` | `Input`: what the user submitted. |
| `src/runtime/mod.rs` | the runtime actor — input loop, dispatch, persistence. |
| `src/runtime/turn.rs` | what happens while a turn or a `!` command runs. |
| `src/runtime/shared.rs` | what `App` and the runtime both reach: state, events, the running turn, its pending request, the router, config, shutdown. |
| `src/runtime/shared/live.rs` | what applies while a turn holds the driver: `/model`, `/agents`, `/exit`. |
| `src/runtime/inbox.rs` | the queue of inputs for the driver, counting the prompts nobody took. |
| `src/commands/mod.rs` | `SlashRegistry`, `CommandContext`, `CommandOutcome`; one module per built-in. |
| `src/capabilities/tools.rs` | the `ToolRegistry` a workspace mounts. |
| `src/capabilities/subagent.rs` | delegated runs — the registry, the concurrency cap, one task per subagent. |
| `src/capabilities/mcp/` | server registry plus the stdio/HTTP client that bridges remote tools. |
| `src/capabilities/memory/` | `memory_read`, `memory_write`, `memory_forget` as agent tools. |
| `src/capabilities/view.rs` | how a tool call renders when only its name and input survive. |
| `src/core/config.rs` | the merged `AppConfig` and its file format. |
| `src/core/session.rs` | JSONL persistence and the recent-session index. |
| `src/core/provider.rs` | routers, clients and the credential-free listings. |
| `src/core/complete.rs` | the one prefix rule every completion list shares. |
| `src/memory.rs` | the memory operations `/memory` and `oven mem` share. |
| `src/platform/` | paths, the tracing subscriber, the local shell envelope. |

`App` has three constructors:

```text
App::builder(root) ─▶ load_config() ─▶ [load_memory()] ─▶ open()            // no session on disk
                                                    ├──▶ open_session(id)   // JSONL under ~/.oven/sessions
                                                    └──▶ query(p)           // open, one prompt, shut down
App::open(root)     ─▶ builder + load_config + load_memory + open; a broken config is an error
```

`load_memory` is opt-in on the builder: skipping it (`oven --amnesia`) mounts
no memory tools and adds nothing to the system prompt.

Only `open_session` reaches an LLM (it builds the interactive router); `open`
uses the non-interactive one. `open_session(Some(id))` resumes that session when
its file exists; `None` — or an id that has none — starts a fresh one with a
uuid v7 id the caller never supplies. Resolving *which* recent session a root
used belongs to the caller: `oven --continue` reads `session::recent_session_id`
before it opens. `AppBuilder::with_config` bypasses the filesystem entirely,
which is how the tests build an app.

Control that never needs the driver is a method on `App`, not an input:
`cancel(turn_id)`, `set_mode`, `respond`, `stop_subagent`, `stop_subagents`,
`rewind`. Reading the published state is either a snapshot (`state()`) or a
`watch::Receiver` (`watch_state()`), plus the common projections — `model`,
`provider_config`, `configured_providers`, `agent_id`, `subagents`, `todos`,
`last_turn_usage`, `session_id`, `history_timed_shared`, `slash_commands`.

`App::shutdown` cancels the running turn and returns how many queued prompts
the runtime dropped without ever running them, so a frontend can tell the user
what it lost on the way out.

`App::prompt` is the convenience path used by both `AppBuilder::query` and the
tests: it subscribes, submits the prompt, then collects text deltas until the
turn completes, fails, cancels, a shell command finishes, or a non-turn
notification arrives. Loop-limit prompts are answered with `LoopLimitDecision::Exit` so a
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

A chat typed while a turn runs is different: `App::steer` parks it on `Shared`
instead of the inbox, so the running turn picks it up when it uploads its next
tool results, and `App::claim_steer` drops one the turn never took. It parks
plain chats only: a slash command or a shell line still waits for the driver.

`core/event.rs` fans events out to every subscriber on its own `UnboundedSender`,
auto-pruning dead ones:

| `AppEventKind` | Emitted when |
| --- | --- |
| `Agent(AgentEventEnvelope)` | streamed text, thinking, tool calls and results, turn start/end — from the driver and every subagent |
| `RequestResolved { request_id }` | the request announced earlier closed: answered, dropped at the turn's end, or replaced |
| `Subagent(SubagentEvent)` | `Focus { id }` — a frontend should open a view on one subagent |
| `HistoryChanged { reason }` | the history was replaced wholesale — rewind, `/clear`, compaction (see [State](#state)) |
| `Shell(ShellEvent)` | `!cmd` started, finished, failed |
| `Compaction(CompactionEvent)` | history compaction started, completed (before/after tokens), failed |
| `Notification { text }` | one-shot backend replies: `/model` confirmations, errors that are not fatal |
| `Error { message }` | turn or IO failure; the runtime keeps running |
| `Exited` | `/exit` accepted |

## Runtime loop

`Runtime` owns the agent plus everything the agent does not: the session store,
the persistence bookkeeping (`persisted_messages`, `persisted_rev`) and the
`RunPolicy` one turn may spend, derived from config so every run the runtime
starts is bounded the same way. The state, event bus, config, router and
subagents it shares with `App` live in `Shared`. `run` selects, with `biased`
priority, on shutdown, the inbox and the subagent wake signal; every input is
logged by kind. It dispatches on the `Input`:

| Input | Path |
| --- | --- |
| `Rewind` | truncate the last turn, persist, publish |
| `Shell` | `run_shell` — a host process, not a turn; an empty command is rejected with a notice |
| `Slash` | `SlashRegistry::run`, then the outcome is applied |
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
  open. Answering it, dropping it when the turn ends, or replacing it with a
  later request publishes `RequestResolved` for that id.
- `TurnContext` carries `Shared` as its `PendingPrompts`: the parked chats
  `App::steer` left, which the turn takes when it uploads tool results.
- `App::cancel(turn_id)` cancels only if `turn_id` is still the running turn.

After the turn: session meta is stamped, trailing agent events are drained,
the turn is persisted, state is synced, and if context usage reached
`compact_threshold` the history is compacted. The phase returns to `Idle`
regardless, so a failed turn never wedges the UI.

## State

`core/state.rs` mirrors the agent into a cloneable `AppState` published on a
`watch` channel, so a frontend renders progress by diffing one small struct
instead of buffering the whole history itself:

```rust
pub enum AppPhase {
    Idle,
    Running { turn_id },
    Awaiting { turn_id },
    Cancelling { turn_id },
    ShuttingDown,
    Compacting,
}
```

The snapshot is the only carrier of levels — mode, model, reasoning effort,
context window, providers, models, subagents, the phase itself — so a frontend
reads them as they stand and no event repeats them. What an `Awaiting` turn
waits for is the agent event that announced the request, not part of the phase,
and `Compacting` marks the driver busy summarizing with no turn to cancel. The
one thing a snapshot cannot say is that the history was replaced, so that goes
out as `HistoryChanged { reason }` (`Rewound`, `Cleared`, `Compacted`,
`External`): a view rebuilds itself from the state and tells a rewind from a
`/clear`.

`AppState` also carries the conversation itself: `history` as `Arc<Message>`
handles with the parallel `history_timestamps` and `history_thinking_ms`, so
publishing state costs refcounts rather than a copy of every turn. It names the
driver (`agent_id`) so an event can be routed to the transcript it belongs to.

`context_tokens` is the input of the last response (cache reads included) plus the
output it added to the history, and `context_window` comes from the router's model
info; both refresh on every `AgentEvent::Usage`, so the ctx% moves during a turn
instead of only at its end. Unknown windows disable the ctx% display and
auto-compaction.

## Sessions

`core/session.rs` persists a conversation as JSONL: one `Record` per line, either a
`Message`, a `TokenUsage` (written once after the final assistant message of a
user turn), a `Thinking` span for the reasoning that preceded an assistant
message (start timestamp plus `duration_ms`), a `TodoList` snapshot or
`SessionMeta`, each with a Unix-ms timestamp, appended as the turn progresses so
a crash loses nothing beyond the last unflushed line. Reading accepts older
formats — bare messages and the `{message, usage}` envelope — with timestamp 0,
and skips lines whose `type` tag it does not know, so a file written by a newer
version still loads.

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

`cwd_latest.json` sits next to the session files: a map of canonical workspace
root to the session id most recently used there, so `--continue` answers one
question with one read.

## Slash commands

`commands/` defines one trait and eight built-ins; the registry is extensible:

```rust
pub struct CommandContext<'a> {
    agent: Option<&'a mut Agent>,
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
`/clear`, `/compact`, `/memory`, `/plan`, `/model` and `/setup` all ask, so they
queue. `SlashRegistry::run_shared` runs a command with no driver at all, and
`Ok(None)` is how it says the command is one that needs it.

| Command | Does |
| --- | --- |
| `/model [id] [effort]` | switches (or reports) the model and reasoning effort; writes the overlay to the user config |
| `/setup name=… api_key=… [model=… base_url=… protocol=…]` | merges a provider overlay and rebuilds routing around it |
| `/compact` | summarizes history into a fresh session file |
| `/clear` | clears history, todos and session |
| `/exit` | emits `goodbye` and `Exited` |
| `/plan [on\|off]` | toggles plan mode; with no argument, reports the current mode and the todo summary |
| `/agents [<name> \| stop <name\|all> \| forget <name>]` | lists subagents, or focuses, stops and drops one |
| `/memory [show <ref> \| rm <ref>]` | lists memories, or shows or removes one. `<ref>` is `workspace/<id>`, `user/<id>`, or a bare id when it is unique |

Commands return a `CommandOutcome` the runtime interprets — `Reply` becomes a
notification, `Passthrough` falls through to an agent turn, `Exit` emits
`Exited`, `ModeChanged` switches the live mode, `ModelChanged` rebuilds routing,
`ProviderChanged` rebuilds the whole router, `Cleared` and `Compact` touch
persistence, `FocusSubagent` publishes a `SubagentEvent::Focus`, and `Memory`
is performed by the runtime against the store the builder loaded. Nothing in
`commands/` performs IO of its own.

`memory.rs` is where the memory outcomes land. It is the single implementation
of `list`, `show`, `remove` and `file_path` that `/memory` and `oven mem` share,
so the slash popup and the CLI subcommand cannot disagree about a reference or
a wording.

`core/complete.rs` holds the prefix rule the completion lists in the TUI select with:
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
vendors require a `base_url`, and per-model metadata can be declared in
`[providers.<slug>.models.<wire-id>]` — context window, output limit and the
capability flags — so a custom gateway still gets ctx%, auto-compaction and
correct request validation. A provider table's own flat metadata is inherited by
every model under it that leaves a field unset.

`save_provider_at` rewrites the file in the canonical format whenever `/model`
or `/setup` changes something.

Config is edited by its own CLI subcommands, so that API is split by intent rather
than by file: `load_file` reads one file with nothing merged in, `save_at`
writes a whole config back, and `remove_provider` / `remove_model` are the
deletions the merge-only `save_provider_at` cannot express — removing the
active provider reselects the first remaining one, and removing the model in
use clears the selection so it falls back to a preset. `empty()` is the
starting point for a file that does not exist yet, because `Default` seeds the
builtin `deepseek` entry and a new file must not inherit it. `provider_toml`
renders the block a save would write, with the API key masked, so a frontend
can preview it.

`provider.rs` also answers what a listing needs without spending a credential:
`provider_catalog` reads the static vendor table (empty for a custom vendor),
and `provider_models` is the real `GET /models`. `verify` sends one token so
`oven model add` can fail before it saves.

Switching providers at runtime (`set_provider`) fills missing fields from the
saved entry and the vendor presets, validates that a key is present, and only
then rebuilds the whole router — a bad key never destroys the working one, and a
subagent holding the old router finishes its request on it instead of racing the
mutation. After the swap the model list is refreshed from `list_models()` with a
short timeout; an auth error surfaces as `API key rejected: …`.

## Tools, MCP and skills

`capabilities/tools.rs` mounts a named set of tools per workspace. The built-ins come from
`oven_agent::BUILTIN_TOOLS` — `file_read`, `file_write`, `file_edit`, `bash`,
`glob`, `grep`, `web_fetch`, `todo_write` and `answer` — and the builder adds
`skill_read` on top. After `AppBuilder::load_memory` — which the CLI calls
unless `--amnesia` is passed — the builder also mounts `memory_read`,
`memory_write` and `memory_forget` and appends the memory catalog to the system
prompt. When `subagents.enabled`, `task`, `task_output`
and `list_models` join as well. An empty config list means the built-in
defaults; unknown names are skipped silently.

`answer` is how the model asks the user something: it publishes a `Question`
(optionally with the answers to choose from) on the turn's question channel and
awaits the reply, which comes back as the tool's result. It needs a frontend, so
it fails with `no user is available to answer the question` on a bare
`Agent`.

## Subagents

`capabilities/subagent.rs` supervises delegated work: one registry, one concurrency cap, one
tokio task per subagent. [`subagents.md`](./subagents.md) is the full design.

`AppBuilder` composes it with the driver, because the two have to share a
router and an event channel from the start:

```text
AppBuilder::build_agent_with_router
  ├── tools ──► Subagents::new(SubagentParts { roles, router, … })
  ├── main Agent::with_router(router, tools + task + task_output + list_models)
  └── AppAgents { main, subagents, events, wake_rx, memory, session }
```

`AppAgents` is what `spawn_runtime` takes. The channel is the reason it exists:
every agent — the driver and each subagent — reports there, so the runtime
drains it in its outer loop while it is idle, not only inside a turn. The
supervisor keeps the matching `wake` sender: a ping with no payload that says
the registry moved, so the runtime can read the snapshot and publish
`AppState.subagents` only when it actually differs.

The roles a subagent may run as are partitioned by what a tool may do rather
than listed by hand, so a tool that joins the read-only set reaches `explore`
without touching `build_roles`: `explore` mounts only `ToolPermission::Read`
tools with a read-only preamble, `general` mounts everything. Neither mounts
`answer`, `todo_write`, `task`, `task_output`, `memory_write` or
`memory_forget` — they speak to the user, manage the caller's plan, or would
nest delegation.

The registry is the single source of truth. `AppState.subagents` mirrors it,
`/agents` reads it, and `task_output` reads it, so a panel and a tool can never
disagree. A subagent is addressed by name (`role#N`, allocated from per-role
counters) or by 1-based position, and at most `MAX_FINISHED` finished entries are
kept for a later look. Subagents are never persisted: only the `task` call and
its result are in the driver's session, so a resumed session shows no
subagents.

Config:

```toml
[subagents]
enabled = true        # mount task / task_output / list_models
max_concurrent = 4    # further spawns wait in `queued`
max_iters = 60        # provider round trips one subagent may take
```

`capabilities/mcp/` declares MCP servers in config (`mcps.<id>`, stdio via
`command`/`args`/`env`, or streamable HTTP via `url`/`headers`) and connects them
at agent build time. `McpRegistry` validates each declaration — a duplicate id,
or one with neither a command nor a url, is rejected — and each server's
`tools/list` is bridged into the agent as `<server_id>_<tool_name>`, so the
model calls them like any other tool. The protocol sits behind two small traits
— `McpCaller` (one `tools/call`) and
`McpConnector` (connect a configured server and list its tools) — which lets
tests mock the server side with mockall instead of spawning a process, and lets
`AppBuilder::with_mcp_connector` inject one. A server that fails to connect
fails the build rather than starting without its tools.

Skills are deliberately *not* tools: they contribute system-prompt guidance and
are read through `skill_read`, discovered from `~/.oven/skills` then the
project's `.oven/skills` (later paths override).

## Shell mode

An input starting with `!` is executed as a host command in the workspace root
with a 300 s timeout — no LLM turn is involved. `run_shell` reuses the same
select loop as a turn, so `App::cancel` stops the process through a
`CancellationToken`. `platform/shell.rs` then formats the result into a `<local-shell>`
envelope (command, exit code, stderr section, output) and pushes it into the
history as a user message, so the model sees what the user just ran. The same
envelope is what `LocalShell::try_parse` understands when it arrives back from
the model.

`core/mention.rs` is a separate utility for the frontend: a `nucleo`-backed fuzzy
search over the workspace files with background rescans, so `@` completion never
blocks the UI thread. It never touches the agent.

## Tests

Unit tests live in-file (`core/session.rs`, `core/config.rs`,
`core/provider.rs`, `runtime/shared.rs`, `runtime/inbox.rs`, `commands/*`,
`capabilities/tools.rs`, `capabilities/memory/*`, `platform/*`,
`capabilities/mcp/registry.rs`, `capabilities/mcp/client_test.rs`). The
runtime's behaviour — input ordering, queued commands, mid-turn model switches,
compaction, rewinds, session switching, approval and loop-limit handling,
steering, subagent supervision — is covered by `runtime/runtime_test.rs` with a
mocked provider. `tests/` holds the integration
paths: config merging (`config.rs`), MCP wiring through a mock
connector (`mcp_client.rs`), and tool and skill mounting (`tools_mcp.rs`), plus
`public_api.rs`, which pins the paths `oven-app` re-exports so moving a module
behind a new layer cannot silently move a public one. Nothing waits on
wall-clock sleeps, and timeouts are always explicit.

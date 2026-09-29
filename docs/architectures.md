# Oven protocol

Oven splits four concepts:

```text
Command  ──→  Runtime  ──→  Event Stream
                         └─→  State

Message  ←──────────────→  History / Session
```

| Concept | Meaning |
| --- | --- |
| **Command** | A request to do something |
| **Message** | What is in the LLM conversation |
| **Event** | What happened while running |
| **State** | What the system is now |

Commands never contain events. Events never contain commands. Turn streaming is not the same as session history.

```text
                        ┌──────────────┐
                        │   oven-tui   │
                        └──────┬───────┘
                               │ App::submit · cancel · set_mode · respond
                               ▼
                     ┌──────────────────┐
                     │   App Runtime    │
                     │                  │
                     │   AppState  ◄────┤ watch
                     │       ▲          │
                     │       │          │
                     │     Agent        │
                     └───────┼──────────┘
                             │ AgentEventEnvelope
             ┌───────────────┼──────────────┐
             ▼               ▼              ▼
         Lifecycle         Stream          Tool
```

## Crate ownership

| Crate | Owns |
| --- | --- |
| `oven-llm` | `Message`, `Usage`, provider I/O |
| `oven-agent` | `Agent`, `RouterHandle`, `TurnContext`, `RunPolicy`, turn execution, tool protocol, `AgentEvent`, `EventSink`, `Selection` (the mode and model the next step runs with), the `RequestSink` a turn asks the user on, history/todo domain models, the `SubagentSpawner` protocol and its `NodeInfo`/`NodeStatus` vocabulary, provider retry decoration |
| `oven-host` | Workspace filesystem access, path confinement, process execution, command-output decoding, directory walking, size-based log rotation |
| `oven-app` | `App`, `AppBuilder`, `Input`, `AppEvent`, `AppState`, app runtime actor, session persistence, local-shell orchestration, subagent supervision, tracing subscriber install |
| `oven-tui` | render events and state; send commands |

`oven-host` is infrastructure, not the app actor. The app runtime owns application state and command dispatch; `oven-host` only provides reusable capabilities with no dependency on Agent or App domain types.

TUI internals are documented in [`oven-tui.md`](./oven-tui.md), the app layer in
[`oven-app.md`](./oven-app.md), and subagents — with the seams they leave for
loop and graph engineering — in [`subagents.md`](./subagents.md).

The dependency direction is:

```text
oven-tui ──► oven-app ──► oven-agent ──► oven-llm
                   │          │
                   └──────────┴──► oven-host
```

`oven-host` may depend on operating-system and third-party implementation crates such as Tokio and `ignore`, but those types do not appear in its public API. Consumers use host-owned types such as `WalkEntry`, `PathError`, and `CommandError`. Pattern matching remains an Agent concern.

## Infrastructure boundary

`oven-host` isolates external side effects from Agent orchestration:

| Capability | Host API | Agent/App responsibility |
| --- | --- | --- |
| Workspace paths | `resolve_within` | Parse tool arguments and map errors to domain errors |
| File access | `write` | Choose paths and content; read files and map errors to domain errors |
| Process execution | `run_shell_command` | Choose command, timeout, cancellation, and event formatting |
| Output decoding | `decode_command_output` | Render decoded stdout/stderr and exit status |
| File discovery | `walk_dir` (files only), `walk_all` (+ directories), `WalkEntry` | Apply glob/grep semantics and result limits |
| Log files | `RotatingFile` | Install the tracing subscriber at process start (`oven-app::log`); emit spans from agent/app |

The host facade deliberately does not know about `AgentError`, `Tool`, `AgentEvent`, `AppEvent`, or `AppState`. This keeps the infrastructure reusable and prevents a dependency cycle.

`oven-agent` retains the `Tool` trait because it describes the Agent protocol. Concrete tools adapt that protocol to host capabilities:

```text
oven-agent::Tool
  ├── FileReadTool / FileEditTool / SkillReadTool ──► tokio::fs
  ├── FileWriteTool / FileEditTool                ──► oven-host write
  ├── BashTool                                    ──► oven-host process
  ├── GlobTool / GrepTool                         ──► oven-agent matching + oven-host walk
  └── TaskTool / TaskOutputTool                   ──► oven-agent SubagentSpawner (oven-app implements it)
```

`Tool::run(&self, args, cx: &TurnContext)` receives the whole run context rather
than a bag of options, so a tool that starts work of its own — a subagent, a
nested loop — inherits the run it was called from instead of guessing. Tools
mount as `Arc<dyn Tool>`: the driver and every subagent it spawns share one
instance each, so spawning never reconnects an MCP server.

## App facade

`App` is the only public app-runtime handle. `AppHandle` is gone. `AppBuilder` loads config, skills, tools, and MCP, then `open()` / `open_session()` spawns the actor. This app runtime is distinct from the `oven-host` infrastructure crate: the former coordinates commands and state, while the latter performs isolated filesystem and process operations.

```text
AppBuilder ──open──► App ──submit()──► inbox ──► Runtime
                     │
                     ├── cancel() · set_mode() · respond() · stop_subagent(s)()  → Shared
                     ├── subscribe()    → AppEvent
                     └── watch_state()  → AppState
```

## IDs

```text
AppId
 └── AgentId            the conversation driver, or a subagent it spawned
       └── TurnId          created by whoever runs the turn
             └── ToolCallId  created by the agent
```

IDs start at 1. There is no `Default` sentinel of `0`.

A turn is an app-level user request. The runtime allocates `TurnId` and passes it into `Agent::run` via `TurnContext`; a subagent's spawner does the same for its own turn. `TurnContext` also holds the run's `RunPolicy` (what one run may spend) and the agent's own `Selection` (mode and model), so `App::set_mode` / `/model` can take effect at the next agent step without waiting for `&mut Agent`.

`AgentId` is ordered, which is what lets a frontend keep one view per subagent in a `BTreeMap` with a stable order.

---

# Inputs

TUI / CLI → runtime, through `App::submit(text)`, which classifies the text once with the slash registry the runtime shares:

```rust
pub enum Input {
    Chat(String),
    Shell(String),
    Slash { name: String, args: String },
    Rewind,
}
```

Classification never happens twice, so callers never sniff strings: `submit` returns the `Input` it sent, and the TUI draws it accordingly. A trimmed body starting with `/` and naming a registered command is `Slash`; an unknown `/x` stays `Chat`.

There is no `SetModel` / `SetProvider` / `ClearSession` input. Those mutations are slash commands (`/model`, `/setup`, `/clear`). Control that needs no conversation driver is a method on `App`, applied by the caller: `cancel(turn_id)`, `set_mode`, `respond`, `stop_subagent`, `stop_subagents`, `shutdown`.

A `Shell` input is a **local shell request**, not a slash command and not an LLM turn. The app runtime invokes `oven-host::run_shell_command` in the workspace root (bash on Unix, falling back to sh; PowerShell on Windows). `oven-host` owns process management and output decoding; the app retains timeout/cancellation choices, shell events, exit-code mapping, and persistence. It commits one user message describing the command and its result.

## Mid-turn dispatch

A running turn holds `&mut Agent` exclusively. `App` splits what it is given on whether that borrow is needed:

| What | While a turn is running |
| --- | --- |
| `cancel(turn_id)` | cancel immediately, only if `turn_id` is the running turn |
| `set_mode` | apply immediately via the shared `Selection` + `AppState` |
| `respond(request_id, response)` | answers the turn's pending request; phase returns to `Running` |
| `Slash /model …` | apply immediately via `RouterHandle` + `Selection` |
| `Slash /agents …`, `Slash /exit` | apply immediately: neither asks for the agent, which is what `CommandContext::agent()` reports |
| `Rewind` | queue; emit `Notification` |
| other `Input` (chat, other slash, bang-shell) | queue; slash commands get a `Notification` |
| `shutdown` | cancel the turn and exit |

The mode and model live in a `Selection` the agent and its `TurnContext` share, so they can change without `&mut Agent`. The agent re-reads them at each step. `TurnContext` also carries the `RequestSink` (`TurnContext::with_requests`), so a turn asks the user through it and consumes the reply directly. `RouterHandle` (`Arc<RwLock<Arc<Router>>>`) is the same idea for the router: a cheap snapshot is enough to qualify a `/model` id while the turn is in flight. Router mutation (`/setup`) still needs `&mut Agent` via `Agent::replace_router`, so it waits.

## User requests

A turn asks the user for three things — approve a tool call, keep going past the
iteration cap, answer a tool's question — and they are the same exchange: one
request out, exactly one reply back. They share one id type, one sink and one
response enum:

```rust
pub enum UserRequest {
    ApproveTool { call_id: ToolCallId, name: String, view: ToolView,
                  responder: oneshot::Sender<ApprovalDecision> },
    LoopLimit { max_iters: usize, responder: oneshot::Sender<LoopLimitDecision> },
    Question { question: Question, responder: oneshot::Sender<AnswerResponse> },
}

pub enum UserResponse {
    Approval(ApprovalDecision),
    LoopLimit(LoopLimitDecision),
    Answer(AnswerResponse),
}

pub trait RequestSink: Debug + Send + Sync {
    fn submit(&self, turn_id: TurnId, request: PendingRequest) -> bool;
}
```

`TurnContext::approve` / `ask` / `request_loop_continue` each mint a
`UserRequestId`, hand a `PendingRequest` to the sink and await the reply on one
private `wait`, which returns `None` when the turn is cancelled. Without a
sink — a subagent, a headless run — a tool call is refused, a question is an
error and the iteration cap stands, so nothing parks on a reply that will never
come.

The app's `Shared` is the sink. `submit` stores the one `PendingRequest` the
turn is parked on, moves the phase to `Awaiting` and publishes the request as
the event a frontend draws, all before the agent starts waiting. `App::respond`
answers it by id: a reply whose id does not match, or whose `UserResponse` kind
does not match the request, is dropped and the request stays open.

A command that never asks for the agent — `/agents`, `/exit` — is applied mid-turn
instead of queued: `CommandContext::agent()` returns `AgentBusy` for the ones that
do need it, and `App::submit` reads that as "wait". Queued inputs drain after the
turn ends, in arrival order. Keyboard mode toggle is `App::set_mode` and applies live; `/plan` is a slash command and therefore waits until the agent is free.

---

# Events

## Agent events

Emitted during one LLM turn. History and model stay on the committed `Message` / `AppState`. Todo and usage updates are the exception: the agent emits `TodosChanged` / `Usage` as soon as a provider response reports them, and a frontend consumes them where they land — the checklist widget from `AgentEvent::TodosChanged`, the context gauge's prompt-side tokens from `AgentEvent::Usage`.

```rust
pub struct AgentEventEnvelope {
    pub agent_id: AgentId,
    pub turn_id: TurnId,
    pub event: AgentEvent,
}

pub enum AgentEvent {
    Turn(TurnEvent),
    Stream(StreamEvent),
    Tool(ToolEvent),
    TodosChanged { todos: TodoList },
    Usage { usage: Usage },
}
```

Events are not numbered and not timestamped: arrival order on the subscriber
channel is the order things happened in, and a `ToolCallId` is the key a
frontend pairs `Started` / `OutputDelta` / `Finished` with.

```text
TurnEvent     Started | Completed { usage, duration_ms }
              StepStarted { index } | StepFinished { index, stop }
              Cancelled { duration_ms } | Failed { error, duration_ms }
              LoopLimitReached { request_id, max_iters }
StreamEvent   TextDelta { text } | ThinkingDelta { text }
              ThinkingDone { duration_ms }
ToolEvent     ApprovalRequested { request_id, call_id, name, view }
              QuestionAsked { request_id, question }
              Started { call_id, name, view }
              OutputDelta { call_id, stream, text }
              Finished { call_id, result }
```

`StepStarted` / `StepFinished` bound one provider round trip, `index` counting
from 1. The calls inside one step run at the same time — `Started` is reported
for all of them, then `Finished` as each lands — so a response that asks for
three subagents gets three working at once. Approvals are still asked one at a
time, tools that declare `ToolCaps::exclusive` (the ones that rewrite a file
from what they read, and the one that asks the user something) take a turn
each, and the tool results enter the history in the order the model asked for
them, which is the order a provider expects them back in. They are what lets a driver — or a strip on screen — report loop
progress and a stop reason without reading the agent.

`ThinkingDone` closes the thinking window the agent timed: it fires the moment
the reasoning phase ends, ahead of the answer text or the tool call that ended
it, so the transcript never has to guess a duration. `LoopLimitReached`,
`ApprovalRequested` and `QuestionAsked` park the turn until the matching
`App::respond` arrives. All three are projected by `Shared` from the one
`RequestSink`: the turn submits a `UserRequest` and waits, and `Shared`
republishes the request as the event a frontend draws and parks the phase on
`Awaiting`. `QuestionAsked` is the one that originates inside a tool call,
where no event sink is reachable — which is why the request sink, not the
event sink, is what a frontend hears about it on.

`ToolResult` is `Success`, `Failed { error, output }`, `Rejected { reason }`, or
`Cancelled` — not `ok: bool`.

Tool input is not on `ToolEvent::Started`. The UI uses `ToolView`. `view.diff` paints `+/-` lines as a file diff. Full arguments live on the committed `Message`.

Final assistant text is the concatenation of `TextDelta`s (or `TurnOutput` / history), not a duplicated `Done { text }`.

## App events

```rust
pub struct AppEvent {
    pub kind: AppEventKind,
}

pub enum AppEventKind {
    Agent(AgentEventEnvelope),
    Subagent(SubagentEvent),
    HistoryChanged { reason: HistoryChangeReason },
    Shell(ShellEvent),
    Compaction(CompactionEvent),
    Notification { text: String },
    Error { message: String },
    Exited,
}

pub enum SubagentEvent {
    /// Open a view on one subagent. A command asks for it; the frontend
    /// decides what a view is.
    Focus { id: AgentId },
}

pub enum CompactionEvent {
    Started,
    Completed { before_tokens: u32, after_tokens: u32 },
    Failed { error: String },
}

pub enum ShellEvent {
    Started { command: String },
    Finished { command: String, output: String, exit_code: i32 },
    Failed { command: String, error: String, output: String },
}
```

There is no `Idle` event. Turn completion is `TurnEvent::Completed | Cancelled | Failed`. App idleness is `AppState.phase`.

`SubagentEvent` is what a frontend should *do*; `AppState.subagents` is what is
*true*. Subagent turns report as ordinary `AgentEventEnvelope`s carrying the
subagent's `agent_id`, so a view can render one with the same code as the
driver's transcript.

Subscribers get a lossless unbounded channel. `App::state()` / `watch_state()` is the current snapshot.

Publishing is one hop: every turn's `EventSink` is a `BusSink` over the shared
`EventBus`, so a subagent's events reach the frontend while the runtime is busy
driving a turn of its own, or idle between turns, without the runtime
forwarding them. The runtime subscribes to nothing; it is only a publisher.

## Agent API

```rust
agent.run(input, &TurnContext::new(turn_id, cancellation, agent.selection()), &mut sink)
    -> Result<TurnOutput, AgentError>

agent.step(&mut sink, &cx) -> Result<Step, AgentError>
```

`run` is one loop policy over `step`, and `step` is public so a strategy of its
own can drive it: ask, commit the reply, run the tools it asked for, and hand
back a `Step { text, calls, usage }`. `Step::is_final` — a step that asked for
no tools — is what ends the default loop. Tool output is not repeated on the
step; it is the tool-result message the step already pushed into the history,
so a step carrying a huge `file_read` stays cheap. `RunPolicy::max_iters` rides
`TurnContext` because it bounds one run, not the conversation driver: a
subagent gets its own.

The agent owns its mode and model as a `Selection`, a cloneable handle over one shared value, so a caller can update them while the turn holds `&mut Agent`. The agent reads it at the start of every step, and the `TurnContext` a run is given shares the agent's own `Selection`, so tools read the same value. `TurnContext` also carries the `RequestSink`, which is how what a turn asks the user reaches the app and how the reply reaches the running turn without needing `&mut Agent`.

The Agent calls host capabilities through concrete tools, but `oven-host` never emits Agent events directly. Tool implementations translate host results into `AgentError`, `ToolEvent`, and `ToolResult`. A tool receives the `TurnContext` of the run it belongs to, which is where it finds cancellation and, when a frontend is attached, the sink it asks the user through.

`Agent::with_router(RouterHandle, tools)` joins a router someone else owns, and `Agent::replace_router` swaps the snapshot rather than mutating it in place: a reader that captured the old router finishes its request on it, which is what makes a mid-flight `/setup` safe while a subagent is still running.

`EventSink::emit` is synchronous. Production uses `BusSink` (publishing onto the shared `EventBus`); tests use `VecEventSink`.

```text
Event       = streaming / lifecycle
TurnOutput  = function return value
```

---

# State

```rust
pub struct AppState {
    pub phase: AppPhase,
    /// The conversation driver; every other agent is a subagent.
    pub agent_id: AgentId,
    /// Subagents in spawn order, mirrored from the registry.
    pub subagents: Vec<NodeInfo>,
    pub mode: AgentMode,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub provider: ProviderConfig,
    pub configured_providers: Vec<String>,
    /// Shared handles, so publishing a snapshot costs refcounts, not a copy.
    pub history: Vec<Arc<Message>>,
    /// Unix-ms timestamps parallel to `history`, from the session records.
    pub history_timestamps: Vec<u64>,
    /// Thinking duration in ms parallel to `history`; `None` when untimed.
    pub history_thinking_ms: Vec<Option<u64>>,
    pub todos: TodoList,
    pub last_turn_usage: Usage,
    pub context_tokens: u32,
    pub context_window: Option<u32>,
    pub session: SessionState,
    pub models: Vec<(String, String)>,
}

pub enum AppPhase {
    Idle,
    Running { turn_id: TurnId },
    /// The turn is blocked on the user: a tool approval, the loop limit or a
    /// question. What it is waiting for reaches a frontend as the agent event
    /// that announced it.
    Awaiting { turn_id: TurnId },
    Cancelling { turn_id: TurnId },
    ShuttingDown,
}
```

`watch` is *what is true now*, and it is the only place a level lives: phase, mode, model and effort, todos, `last_turn_usage`, context tokens and window, providers (`configured_providers` holds the canonical slugs saved under `[providers.<slug>]`), models and subagents. No event repeats them. A frontend subscribes to the `watch` and reads them as they stand; the composer is busy exactly while `phase.is_active()`.

Events say *that something happened*. The one thing a snapshot cannot say is that the history was replaced wholesale, so it goes out as `HistoryChanged { reason }` with a `HistoryChangeReason` (`Rewound`, `Cleared`, `Compacted`, `External`); the view rebuilds from the state.

`last_turn_usage` is tokens for the most recent agent turn, not a session total. Prompt-side tokens for the context gauge travel with the turn's own `AgentEvent::Usage`, and the checklist with `AgentEvent::TodosChanged`, because a running turn reports them before the snapshot catches up; the frontend takes the snapshot for both once the phase is `Idle`.

UI rule: consume state as truth, events as “something happened”.

| Old event | Now |
| --- | --- |
| `AgentEvent::Done { text, usage }` | `TurnCompleted` + `TextDelta*` |
| `AgentEvent::HistoryCleared` | `AppEventKind::HistoryChanged` |
| `AgentEvent::ModelChanged` | `AppState.model` |
| `AgentEvent::TodoUpdated` | `AgentEvent::TodosChanged`, then `AppState.todos` |
| `AppEvent::Idle` | `AppPhase::Idle` |
| `AppEvent::Rewound { messages, … }` | `HistoryChanged` + the new `AppState` |
| `AppEvent::Notify` | `Notification` |
| `AppEvent::Exit` | `Exited` |

---

# App phase machine

Phase is runtime state. Turn events are facts about one turn. Do not mix them.

```mermaid
stateDiagram-v2
    [*] --> Idle

    Idle --> Running: Prompt (passthrough or bang-shell)
    Idle --> Awaiting: an approval prompt, the iteration cap, or a question
    Idle --> Idle: slash / empty bang / Rewind / set_mode
    Idle --> ShuttingDown: Shutdown

    Running --> Cancelling: Cancel { matching turn_id }
    Running --> Idle: TurnCompleted / TurnFailed
    Running --> ShuttingDown: Shutdown

    Awaiting --> Running: respond
    Awaiting --> Cancelling: Cancel
    Awaiting --> Idle: TurnFailed / TurnCancelled
    Awaiting --> ShuttingDown: Shutdown

    Cancelling --> Idle: TurnCancelled
    Cancelling --> ShuttingDown: Shutdown

    ShuttingDown --> [*]
```

Idle slash commands do not enter `Running`. They update the state and/or emit a `Notification` and stay `Idle`.

A bang-shell `Input::Shell` enters `Running` like an agent turn (so Cancel and queuing work) but emits `AppEventKind::Shell` instead of `AgentEvent`. The agent is not called. On finish the runtime appends a `<local-shell>` user message and persists; it does not emit `HistoryChanged` on the live path.

While `Running`, `set_mode` and `/model` apply immediately and the phase stays `Running`. Other inputs and `Rewind` wait in the inbox. `cancel` while idle is a no-op, and `cancel(turn_id)` only applies if it matches the active turn.

A step that needs a tool approval, that hits the iteration cap, or whose tool asked a question parks the phase on `Awaiting`. What the turn is waiting for is not in the phase — it is the agent event that announced the request — so the phase stays one variant while the prompt on screen changes. The matching `App::respond` returns it to `Running`, and a reject / exit reply, or a cancel, ends the turn.

---

# Turn event machine

Each turn: exactly one `Started`, exactly one terminal event.

```mermaid
stateDiagram-v2
    [*] --> Started

    Started --> Streaming: TextDelta / ThinkingDelta
    Started --> ThinkingDone: ThinkingDone
    Started --> Tool: ToolStarted
    Started --> Completed: no tool calls
    Started --> Cancelled: cancel
    Started --> Failed: provider / loop error

    Streaming --> Streaming: TextDelta / ThinkingDelta
    Streaming --> ThinkingDone: ThinkingDone
    Streaming --> Tool: ToolStarted
    Streaming --> Completed: end of assistant text
    Streaming --> Cancelled: cancel
    Streaming --> Failed: error

    ThinkingDone --> Streaming: follow-up text
    ThinkingDone --> Tool: ToolStarted

    Tool --> Tool: OutputDelta / ToolFinished / ToolStarted
    Tool --> Waiting: ApprovalRequested / LoopLimitReached / QuestionAsked
    Tool --> Streaming: follow-up text
    Tool --> Completed: final assistant message
    Tool --> Cancelled: cancel
    Tool --> Failed: error

    Waiting --> Tool: Respond
    Waiting --> Failed: cancel, or an exit reply to the loop limit

    Completed --> [*]
    Cancelled --> [*]
    Failed --> [*]
```

`ThinkingDone` is not a terminal state: it closes the reasoning window the agent timed, before the answer or the tool call that ended it. Approval, loop-limit and question parks are one state, turn-scoped; the reply arrives as `App::respond` and the turn resumes.

Typical successful sequence:

```text
TurnStarted
  ThinkingDelta*
  ThinkingDone
  ToolStarted → ToolOutputDelta* → ToolFinished
  TextDelta*
TurnCompleted { usage, duration_ms }
```

Then runtime sets `phase = Idle`. `TurnCompleted` is the fact; `Idle` is the phase.

---

# Subagents

A subagent is another `Agent` running its own turn in its own task, over the
driver's router and the same tool instances. `oven-agent` owns the vocabulary —
`SubagentSpawner`, `SpawnRequest`, `NodeHandle`, `NodeInfo`, `NodeStatus`,
`RoleSpec` — and `oven-app::SubagentParts`/`Subagents` owns the machinery. That
split is what lets a tool delegate without knowing how a subagent is built.

```text
task / task_output ──► Arc<dyn SubagentSpawner> ──► Subagents
                                                      ├── registry (the truth)
                                                      ├── Semaphore(max_concurrent)
                                                      └── tokio task per subagent
```

| Concern | How |
| --- | --- |
| Roles | `explore` mounts the read-only tools, `general` mounts everything except the tools that speak to the user, manage the driver's plan, or would nest delegation. Partitioning is by `ToolCaps::permission`, so a new read-only tool reaches `explore` on its own. |
| Addressing | `role#n`, unique for the app run; `1` also means "the first in the listing". |
| Concurrency | A `Semaphore`; a spawn without a permit waits in `Pending` rather than being refused. |
| Cancellation | One token per subagent, cancelled by name, by the turn that spawned it, or by app shutdown. |
| Prompts | A subagent's `TurnContext` carries no request sink, so it can never park waiting on a user. It runs in `AgentMode::Agent`; what it may do comes from its role. |
| Status | The registry is the single source of truth; `AppState.subagents` mirrors it, `/agents` reads it, and `task_output` reads it, so the three cannot disagree. |
| Results | A foreground `task` returns the report as its tool result; a background one returns a name to poll with `task_output`. |
| Persistence | None. Subagents live for the app run; only the tool call and its result are in the driver's history, so `/agents` after a resume shows nothing. |

Subagents are deliberately one-shot: a finished subagent's `Agent` is dropped
and only its report and counters are kept. Revisiting a node — a graph loop —
therefore produces a fresh `AgentId` and `TurnId` by construction, with no
"visit" concept to invent.

---

# Flows

## Normal turn

```mermaid
sequenceDiagram
    participant TUI
    participant Runtime
    participant Agent

    TUI->>Runtime: submit("fix foo") → Chat
    Runtime->>Runtime: TurnId::next()
    Runtime->>Runtime: phase = Running(id)
    Runtime->>Agent: run(input, ctx, sink)
    Agent-->>TUI: TurnStarted
    Agent-->>TUI: ThinkingDelta / TextDelta / Tool*
    Agent-->>Runtime: TurnOutput
    Runtime->>Runtime: persist, snapshot, phase = Idle
    Agent-->>TUI: TurnCompleted
```

## Cancel

```mermaid
sequenceDiagram
    participant TUI
    participant Runtime
    participant Agent

    TUI->>Shared: cancel(turn_id)
    Shared->>Shared: phase = Cancelling(id)
    Shared->>Agent: cancellation.cancel()
    Agent-->>TUI: TurnCancelled
    Runtime->>Runtime: phase = Idle
```

## Slash (no turn)

```text
submit("/model gpt-4o")
  → Slash /model
  → AppState.model updated
  → Notification("model switched…")
  → phase stays Idle
```

The same `submit("/model …")` during `Running` does not wait: `RouterHandle` validates, the shared `Selection` updates, the state changes and a `Notification` fires, and the in-flight turn picks up the new model at its next step.

`prompt()` waits for `TurnCompleted`/`Cancelled`/`Failed`, or `Shell` Finished/Failed, or for `Notification`/`Exited` when no turn started.

## Bang shell

```mermaid
sequenceDiagram
    participant TUI
    participant Runtime
    participant Shell

    TUI->>Runtime: submit("!ls") → Shell
    Runtime->>Runtime: TurnId::next()
    Runtime->>Runtime: phase = Running(id)
    Runtime-->>TUI: Shell(Started)
    Runtime->>Shell: bash/powershell in workspace root
    Shell-->>Runtime: stdout/stderr/exit
    Runtime-->>TUI: Shell(Finished | Failed)
    Runtime->>Runtime: push user envelope, persist, phase = Idle
```

The TUI shows the typed `!` line as a user row and the last 100 output lines as a result row. The committed user message is the parseable envelope so resume/rewind can rebuild the same rows. The agent does not auto-reply.

---

# Invariants

1. `Running(turn_id)` means exactly one active request **of the driver's**: its own agent turn, or a bang-shell command. A subagent's turn runs alongside and never enters `AppPhase`; its progress is `AppState.subagents`.
2. Every driver `AgentEventEnvelope.turn_id` matches `phase.turn_id()` while the phase is `Running`, `Awaiting`, or `Cancelling`. Agent envelopes are not emitted for bang-shell, and a subagent's envelope carries its own `agent_id` and `turn_id` instead.
3. Each agent turn — the driver's or a subagent's — emits exactly one `Started` and exactly one of `Completed | Cancelled | Failed`. Each bang-shell request emits exactly one `Shell::Started` and exactly one of `Finished | Failed`.
4. `ToolFinished` always follows the `ToolStarted` of the same `ToolCallId`. A call that never ran — a tool hidden in Ask mode, an approval the user declined — reports `ToolFinished` alone, because nothing started.
5. While `Running`, `Awaiting`, or `Cancelling`, only `cancel`, `set_mode`, `/model`, `/agents`, `/exit`, `stop_subagent`, `stop_subagents`, `respond`, and `shutdown` are applied immediately. Everything else waits in the inbox.
6. Only the driver's own events feed the frontend's checklist, context gauge and usage readout. A subagent's `Usage` is its own and its `TodosChanged` is not the user's; a frontend routes a subagent's envelope to that subagent's view and nothing else.
7. A subagent's events reach the frontend while the runtime is busy with a turn of its own, or idle between turns: every agent publishes straight onto the shared `EventBus`, so no runtime drain is needed for them to land.

Runtime is a single app actor. It is not the `oven-host` crate:

```rust
struct Runtime {
    agent: Agent,
    shared: Arc<Shared>,      // state, events, config, router, turn control: also held by `App`
    wake_rx: Receiver<()>,    // the registry changed
    root: PathBuf,
    session: Option<SessionStore>,
    slash: Arc<SlashRegistry>,
    persisted_messages: usize,  // agent messages already in the session file
    persisted_rev: u64,         // history revision `persisted_messages` belongs to
}
```

`oven-host` is stateless infrastructure from the app actor's point of view. It owns no session, Agent, event stream, or app state. Its filesystem entry point rejects empty, parent-relative, absolute, and platform-prefixed paths before joining them to a workspace root. This is lexical path confinement; callers requiring protection against symlink traversal must add a canonicalization or symlink policy before treating the workspace as a security sandbox.

```rust
impl Runtime {
    async fn run(mut self, mut inbox: InboxReceiver) { /* dispatch */ }
}
```

`runtime/mod.rs` owns the actor: the input loop that also serves subagent
events, dispatch, and persistence. `runtime/turn.rs` owns what happens while a
turn runs — `start_turn`, `run_shell` and slash commands. `shared.rs` owns what
`App` and the actor both reach, including what applies while a turn holds
`&mut Agent`: cancelling, switching mode, answering a request, `/model`.
`builder.rs` owns construction; `subagent.rs` owns delegated runs.

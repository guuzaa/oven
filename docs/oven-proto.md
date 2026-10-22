# oven-proto

`oven-proto` is the protocol layer: the vocabulary a frontend and the app
runtime share. Everything a frontend can observe or request is named here —
the events that report what happened, the state that says what the system is
now, and the commands that ask for something to be done.

The crate is a **leaf**. It depends on `oven-llm` for messages and tokens and
on serde for the wire format, and on nothing else. No runtime, no filesystem,
no clock, no agent.

That is the whole point. A frontend that speaks this crate — a terminal, a web
page, a remote client — can be written without the agent runtime in its
dependency tree, and the app runtime can be written without knowing how any
of them render what it reports.

```text
oven-llm ──► oven-proto ◄── oven-agent ──► oven-app ──► oven-tui
                  ▲                              │
                  └──────────────────────────────┘
                    the app runtime speaks it
```

## The boundary

Only the `App` facade crosses the protocol boundary. Inside it, nothing is
serialized: the runtime calls the agent, the agent reports through
`EventSink`, and the bus fans the events out to whoever subscribed.

```text
frontend ──JSON──► transport ──Rust call──► App ──► Runtime ──► Agent
        ◄──JSON──              ◄──Rust call──┘      ◄──EventSink──┘
```

A transport therefore does exactly two things: turn what it received into a
`Command`, and turn what the app published into bytes. It never has to know
what a tool is, what a turn is, or how the agent loop works.

## The four nouns

| Noun | Type | Direction | What it is |
| --- | --- | --- | --- |
| Command | `Command` | in | A request to do something |
| Event | `AppEvent` | out | What happened while running |
| State | `AppState` | out | What the system is now |
| Result | `CommandResult` | out | What one command did |

Commands never contain events. Events never contain commands. Turn streaming
is not the same as session history.

### Commands

One enum covers every request a frontend can make, so a transport carries a
single message type in each direction and the set of things a frontend may ask
never grows past what the app already answers.

```rust
pub enum Command {
    Submit { text: String },
    Steer { text: String },
    Rewind,
    Cancel { turn_id: TurnId },
    CancelCompaction,
    SetMode { mode: AgentMode },
    Respond { request_id: UserRequestId, response: UserResponse },
    StopSubagent { id: AgentId },
    StopSubagents,
}
```

`App::apply` applies one:

```rust
let outcome = app.apply(Command::Submit { text: "fix foo".into() }).await?;
```

`Submit` classifies the text once — a chat, a `!` shell line or a slash
command — and reports the classification as `CommandResult::Submitted(Input)`,
so a frontend draws it the way the runtime will treat it. `Steer` parks a
chat typed while a turn is running; the app refuses it when the driver is
idle, and says so with `CommandResult::Refused`. `Respond` is refused the
same way when the request it answers was already closed.

### Events

`AppEventKind` is what a frontend reacts to. The agent's own events ride
inside it, tagged with the agent and the turn they belong to, so a frontend
routes an envelope to the conversation it belongs to and ignores the rest.

```text
Agent(AgentEventEnvelope)      an agent's turn, stream or tool event
RequestResolved { request_id } the request a turn was waiting on is closed
Subagent(SubagentEvent)        something to do about a subagent
HistoryChanged { reason }      the history was replaced wholesale
Shell(ShellEvent)              a `!` command ran
Compaction(CompactionEvent)    the history was summarized
Notification { text }          something to tell the user
Error { message }              something went wrong
Exited                         the app is done
```

`AgentEvent` is the agent's half of the same vocabulary: `Turn`, `Stream`,
`Tool`, `TodosChanged` and `Usage`.

### State

The state a frontend reads is split in two, because it changes at two very
different rates.

```rust
pub struct AppState {
    pub levels: AppLevels,          // small, changes constantly
    pub history: Vec<Arc<Message>>,  // large, changes when the talk does
    pub history_timestamps: Vec<u64>,
    pub history_thinking_ms: Vec<Option<u64>>,
}
```

`AppLevels` holds the phase, the mode, the model, the reasoning effort, the
provider, the checklist, the usage and the context gauge. A frontend keeps it
current from a stream of updates, and it is cheap enough to send on every
change.

`AppState` is the whole picture, the conversation included. A frontend reads
it **once**, when it connects, and rebuilds its transcript from it — the same
way the terminal seeds its transcript from the shared history.

The conversation is held as `Arc<Message>`, so publishing the state costs a
refcount per message rather than a copy of the conversation.

The state has a serde form like the rest of the vocabulary, so a transport
forwards it as bytes instead of mirroring it by hand.

### The credential never crosses

`AppLevels::provider` is a `ProviderView`, not the app's `ProviderConfig`:

```rust
pub struct ProviderView {
    pub name: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub protocol: Option<ProviderKind>,
    pub has_api_key: bool,        // whether one is configured, never which
    pub reasoning_effort: Option<ReasoningEffort>,
}
```

The API key stays in the app layer. A frontend is told whether a credential
is configured, so it can offer `/setup`; it never receives the credential
itself. `App::provider_view` hands out that same view, and the configuration
that carries the key never leaves the app layer.

## The wire format

serde's defaults, with two conventions worth stating:

- An enum that carries its own fields is **internally tagged**:
  `#[serde(tag = "kind", rename_all = "snake_case")]`.
- An enum with a variant that wraps *another enum* is **adjacently tagged**:
  `#[serde(tag = "kind", content = "data", rename_all = "snake_case")]`.

```json
{"kind":"submit","text":"fix foo"}
{"kind":"respond","request_id":4,"response":{"kind":"approval","value":"approved"}}
{"kind":"tool","data":{"kind":"started","call_id":7,"name":"read","view":{"summary":"Read src/lib.rs","collapse":true}}}
{"kind":"notification","data":{"text":"model switched"}}
```

Ids are `u64` counters, so a frontend can order what it receives without
asking the app layer.

`Record` is the one exception to the `kind` tag: it is the session file's
own format, one JSONL line each, and it has been tagged `type` since the
first session file was written. Changing it would invalidate every session on
disk.

## What the crate holds

| Module | Holds |
| --- | --- |
| `command` | `Command`, `CommandResult` |
| `app_event` | `AppEvent`, `AppEventKind`, `AppId`, `ShellEvent`, `CompactionEvent`, `SubagentEvent` |
| `event` | `AgentEvent`, `AgentEventEnvelope`, `TurnEvent`, `StreamEvent`, `ToolEvent`, `ToolResult`, `CallOutcome` |
| `state` | `AppState`, `AppLevels`, `AppPhase`, `SessionState`, `HistoryChangeReason`, `ProviderView` |
| `interaction` | `Question`, `QuestionOption`, `UserResponse`, `ApprovalDecision`, `LoopLimitDecision`, `AnswerResponse` |
| `subagent` | `NodeInfo`, `NodeStatus` |
| `todo` | `TodoList`, `TodoItem`, `TodoStatus` |
| `record` | `Record`, `SessionMeta` |
| `id` | `AgentId`, `TurnId`, `ToolCallId`, `UserRequestId` |
| `mode` | `AgentMode`, `ToolAccess` |
| `permission` | `ToolPermission` |
| `view` | `ToolView` |
| `error` | `AgentError`, `MAX_ITERS_EXCEEDED` |
| `sink` | `EventSink`, `NullSink`, `VecEventSink` |
| `input` | `Input` |

`oven-agent` and `oven-app` both re-export what they used to own, so
`oven_app::AppEvent` and `oven_agent::AgentEvent` still resolve and no
public path moved.

## Two rules for the vocabulary

**The clock is the caller's.** `NodeInfo::elapsed_ms(now_ms)` takes the
current time rather than reading it. A protocol crate that pulled in the
host's clock would drag filesystem infrastructure into every frontend that
speaks it; passing `now_ms` keeps the crate a leaf and makes the answer
reproducible.

**The session format is part of the protocol.** `Record` and `SessionMeta`
describe what a session file holds. A frontend that reloads a session reads
the same records the runtime wrote, so the format is named here rather than
buried in the app layer.

## Using it

The terminal uses it today, in process:

```rust
let events = app.subscribe();               // AppEvent
let state = app.watch_state();              // AppState
app.apply(Command::Submit { text }).await?; // Command
```

A web backend adds a transport on top and changes nothing below it:

```rust
async fn handle(socket: WebSocket, app: App) {
    let mut events = app.subscribe();
    let mut levels = app.watch_state();
    // read commands from the socket, apply them with app.apply
    // forward events and the levels to the socket
}
```

The transport is the only thing that has to know about JSON, websockets or
HTTP. Everything below it speaks Rust.

## Extending it

Adding a command is a change to `Command` and to `App::apply`, and nothing
else — a frontend that does not know the new variant is unaffected.

Adding an event is a change to `AppEventKind` and to whatever raises it.

Changing the shape of an existing variant is a breaking change: the wire
format is the contract, and a frontend written against the old shape will
misread the new one. Add a variant instead.

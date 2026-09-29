# oven-tui

`oven-tui` is the terminal presentation layer. It renders state and events, owns
keyboard/mouse handling, and sends commands back to `oven-app`. It makes no
business decisions: the runtime still classifies composer text, manages turns,
and owns persistence.

Nothing in this crate is reachable except the `oven` binary:

```text
src/bin/oven-cli.rs  →  Cli::parse().run()  →  Ui::new(app).run()
```

## Entry chain

| File | Role |
| --- | --- |
| `src/bin/oven-cli.rs` | `#[tokio::main]` entry that parses the CLI and runs it. |
| `src/cli.rs` | clap parsing plus mode selection. |
| `src/ui.rs` | event loop, cross-component routing, drawing. |

`Cli` has three flags: `--cd/-C` (workspace root), `--session/-s` and
`--continue/-c` (mutually exclusive session selection; an explicit id wins,
otherwise the newest session for this root), and `--query/-Q`.

`Cli::run` dispatches to one of three modes:

| Condition | Behavior |
| --- | --- |
| `--query QUERY` present | headless: `App::query`, print the response, exit. No TUI is started. |
| `--query` absent and stdin/stdout are both TTYs | interactive: `Ui::run` |
| `--query` absent and either side is not a TTY | print usage, exit code `2` |

Warnings from config loading and session resolution go to stderr and never abort
startup. On interactive exit the resolved session id is printed as
` oven -s {id}` so it can be copied back into a shell.

## Event loop

`Ui` owns the `App`, an `mpsc::UnboundedReceiver<AppEvent>`, and one widget per
screen region. The loop is a `tokio::select!` over three branches:

| Branch | Purpose |
| --- | --- |
| 80 ms tick, enabled only while `busy` or a reply is showing | drives the spinner and expires the reply toast |
| crossterm `EventStream` | keys, pastes, mouse |
| `AppEvent` receiver | backend facts |

Terminal events are converted first: a bracketed paste arrives as one
`Event::Paste`, but Windows console input has no bracketed paste, so
`paste_burst::coalesce` re-assembles a run of printable keys into a single paste
(one `Enter` per pasted line would otherwise submit once per line).

After an app event is applied, `drain_events` repeatedly `try_recv`s until the
channel is empty, so a burst of deltas costs one draw instead of one draw per
delta. A disconnected channel settles the live transcript response and clears
`busy`.

The levels come from the app's `watch` instead: on every change `sync_state`
reads `busy` from `phase.is_active()` and takes `mode`, the subagent strip, the
status bar's model and window, the input's providers and models, and — only
between turns — the checklist and the usage readout, since a running turn
reports those itself. It then sends whatever was queued once the app is idle.

`apply_event` maps each event onto four things:

- `Ui`'s own `state.agents` / `rewinding` / `quit` / `esc_confirm_until`, the
  viewer (`focus`), and the strip's copy of the registry
- transcript, status and todos widgets via `on_event` — and, for an
  event whose `agent_id` is not the driver's, only that subagent's transcript
- overlay prompts (`OverlayPrompt`) for tool approval, loop limit, and a
  question from the `answer` tool that is answered either by picking an offered
  option or by typing into the composer. The prompt closes when
  `RequestResolved` names the request it was opened for. A local answer closes
  it immediately; the event covers a reply or a drop that happened elsewhere,
  including the turn ending while the request was still open
- `pending`, the messages queued while the backend is busy

### Queueing

Typing while busy is still allowed; pressing Enter on a normal message enqueues
it (`Action::Queue`) and a one-line `queued · …` row appears. When the backend
reports idle, `maybe_flush` sends each queued message in order (`send_each`
stops at the first failure and keeps the remainder). `/model` and `/setup` are
never queued — they submit immediately, because the runtime applies them live.

### Esc behaviour

`EscAction::new` resolves the overloaded Esc key in a fixed order: pop the queue
into the composer → cancel a running turn → rewind the last user message →
ignore. The rewind text comes from the latest User or Shell row in the
transcript; shell prompts come back in their bang form. During a rewind the
`rewinding` flag blocks a plain Enter until `HistoryChanged` arrives, so the
user cannot submit before the backend has truncated history; that event then
rebuilds the transcript from the truncated history.

The resolved action only fires when Esc is pressed twice inside
`ESC_CONFIRM_WINDOW` (1 s), so a stray press cannot cancel a turn or rewind the
transcript. The first press arms it and the status bar switches to the
`EscArmed` hint; the arm expires on the next tick after the window closes, and
any other key drops it. An action the key cannot perform (`Ignore`) never arms
anything.

### Component contract

`components/component.rs` defines the shared surface:

```rust
pub trait Component {
    fn handle_key(&mut self, key: KeyEvent, state: &State) -> KeyResult;
    fn handle_mouse(&mut self, mouse: MouseEvent, state: &State) -> KeyResult;
    fn on_event(&mut self, ev: &AppEvent) {}
    fn draw(&mut self, f: &mut Frame<'_>, area: Rect, state: &State);
}

pub enum KeyResult { Ignored, Handled, Action(Action) }
pub enum Action {
    Quit, Cancel,
    Submit(String), Queue(String),
    QuietSubmit(String),  // slash command that must not touch the transcript
    Notify(String),
}
```

`Ignored` bubbles to the next component (transcript, then input). `Handled`
stops. `Action` is executed by `Ui`, never by a widget.

`State` is the only shared render state: `busy`, `mode`, and `frame` (a tick
counter used for animations).

## Layout

`components/layout.rs` splits the screen into named rows, top-down:

```text
┌──────────────────────────┐
│ transcript (Min 1 row)   │
├──────────────────────────┤
│ queue       (0 or 1)     │
├──────────────────────────┤
│ agents      (0..=3)      │
├──────────────────────────┤
│ todos       (0..=6)      │
├──────────────────────────┤
│ input        (1..=10)    │
├──────────────────────────┤
│ overlay      (0..=n)     │
├──────────────────────────┤
│ status       (1)         │
└──────────────────────────┘
```

A submitted prompt is appended to `Transcript` before any turn events arrive.
It is therefore the first row of its turn and shares scrolling, selection, and
mouse coordinates with thinking, tool, and response rows. While the view follows
the tail, `Transcript` projects that active row at the top of its own viewport
and reserves the corresponding internal height for response content. Scrolling
up, or receiving `Completed`, `Cancelled`, `Failed`, or a dropped backend,
removes only this projection: the prompt row remains in place in the transcript.

`App::submit` classifies the text into an `oven_app::Input`, and `push_submitted`
draws that: `Chat` starts a User turn, `Shell` starts a Shell turn, and
registered control commands (`/model`, `/setup`, `/clear`, …) and `Rewind` stay
out of the transcript. Queue entries create their row only
when they are actually sent.

Startup and every `AppEventKind::HistoryChanged` rebuild the transcript directly
from `App::history`; no user row is promoted out of history. On a short terminal
the transcript keeps one row whenever possible, then status; optional bands are
clamped to the remaining height.

`components/terminal.rs` owns the raw-mode lifecycle: raw mode, alternate
screen, mouse capture, bracketed paste on `setup()`, and the inverse on
`restore()`. `components/theme.rs` is a flat list of style functions — one per
line kind, border state, and status segment.

## Components

| Module | Role |
| --- | --- |
| `input.rs` | multi-line composer; dispatches to the four overlays; dynamic height; border colour encodes mode |
| `question_prompt.rs` | the `answer` tool's question: options plus an `Other…` row that hands the composer the keystrokes |
| `status.rs` | bottom status row plus the transient reply toast |
| `transcript/` | scrolling conversation, streaming, selection, tool grouping |

| `setup_wizard.rs` | staged provider configuration |
| `model_picker.rs` | two-stage model + reasoning-effort picker |
| `slash_command_popup.rs` | `/` command completion |
| `file_mention_popup.rs` | `@` file completion |
| `choice_popup.rs` | the approve/reject and continue/exit modals |
| `queue.rs` | the queued-message row |
| `agents.rs` | the subagent strip and the viewer's hint row |
| `todos.rs` | read-only checklist |
| `list.rs` | shared list primitive (cycling, `▸` marker, titled header) |
| `shell.rs` | `!` shell-mode detection and prompt styling |
| `transcript/collapsible.rs` | expand/collapse state with pinning, nested under `transcript/` |
| `paste_burst.rs` | Windows paste reconstruction |

### Input

`InputView` wraps `tui-textarea` and grows from 1 to 8 rows. Height is computed
before drawing (`height(area_width)`) and fed back into `layout::split`. Below a
minimum width the rounded border is dropped rather than overlapping the prompt.

Any single overlay is active at a time, in priority order
(`overlay()`): `Setup` > `Model` > `Slash` > `Mention`. While one is open it
receives every key. The model picker also writes back to the composer, mirroring
the line it is editing (`/model <filter>`, then `/model <id> <effort>`) so the
typed text stays in the input box; the setup wizard draws its own prompt in the
box, and the popups leave the frozen line alone.

Border colour encodes mode: shell (`!…`) > Plan > Ask > has text > idle.

Mouse wheel over the input scrolls a multi-line draft. It moves the cursor with
`CursorMove` rather than `TextArea::scroll`, because the latter can scroll the
top row past the content and leave the box mostly blank.

### Slash and mention popups

Both are prefix filters over a `Vec<(name, description)>` and share `list.rs`.
Tab fills the completion; Enter fills when the token is only a prefix and
submits when it is an exact match; Esc closes. At most `MAX_LIST_ROWS` (6) rows
are shown.

The mention popup parses the `@token` under the cursor. The `@` must follow a
whitespace boundary (so an email address is not a mention), a trailing `/` means
the entry is a directory and stays open for drill-down, and insertion is
splic-aware so text after the token is preserved.

### Completion rule

Slash names, model ids and provider names all select through `oven_app::complete`:
an ASCII-case-insensitive prefix, where an empty query keeps the whole list.
`matches_model` also tries the wire id a slug resolves to, so `gpt` finds
`openai/gpt-4o`. The TUI only renders the indices the app selected.

### Model picker

`/model` and `/model <fragment>` open a modal instead of submitting. Stage one
filters the model list by the characters typed, which are drawn in the composer
instead of a popup header; Tab completes the highlighted id into that line and
Enter promotes to stage two, which picks a reasoning effort and composes
`/model <id> <effort>`. `keep current` emits `/model <id>` with no effort. Two or
more arguments skip the picker and submit directly, which keeps a manual
fast path.

### Setup wizard

`/setup` with no arguments walks five stages: provider name (with `keep
current` and `custom`) → custom gateway id → base URL → protocol → api key. The
provider and protocol stages are lists narrowed by typed characters, shown in the
input box, with Tab completing the highlighted id into it. Text stages echo what
is typed; the api key stage shows `*` per character. `requires_
new_key` compares the draft provider against the current one and the configured
slug set, so switching to an already-configured provider keeps its saved key.
The wizard never stores anything itself — it composes `/setup name=… api_key=…`
and submits it as a command. `display_user_input` redacts `api_key=…` before the
line reaches the transcript or the persisted history.

### Status bar

One row: `model [effort] · mode · root · usage · [ctx n%]`, with a spinner
prefixed while busy. The trailing hint is context-sensitive:

| Hint | Text |
| --- | --- |
| `Idle` | `shift-tab mode · enter send · alt-enter newline · esc undo` |
| `Busy` | `shift-tab mode · esc cancel · enter queue` |
| `Slash` | `tab fill · enter · esc` |
| `Modal` | `enter · esc` |
| `Approval` | `enter/y approve · esc/n reject · ctrl-c cancel` |
| `LoopLimit` | `enter/y continue · esc/n exit · ctrl-c cancel` |
| `EscArmed` | `esc again to confirm` |

When the row does not fit, the hint is dropped and the left side is truncated
with `…` by display width.

App notifications become a toast: 3 s TTL, anchored bottom-right over the
transcript and clamped to that area, with a 150 ms blank flash when a new reply
replaces a visible one so the change is noticeable without reading it.

## Transcript

The transcript is a row model, not a growing list of wrapped lines:

```rust
struct Row { kind: LineKind, text: String, collapsible: Option<Collapsible>, headers: Vec<Header> }
```

`transcript/widget.rs` keeps two wrap buffers: `wrapped` for committed rows and
`wrapped_stream` for the in-flight delta. Streaming text is appended to
`wrapped_stream` only, so an in-progress answer never rewraps history and never
yanks a view the user has scrolled away. `rewrap_all` is only triggered by a
real content or width change and clears the selection.

Row kinds (`transcript/kinds.rs`) each carry a two-column gutter and a style:

| Kind | Gutter | Notes |
| --- | --- | --- |
| `User` | `› ` | framed in the composer's rounded border, gutter inside the frame; selection copies only the text |
| `Shell` | `$ ` | framed like `User`, in shell colors, with `$ ` inside the frame |
| `Text` | `∙ ` | assistant prose; gutter drawn on the first line only |
| `Thinking` | `  ` | header holds the duration, body is collapsed |
| `Tool` | `  ` | burst summary row |
| `Diff` | `  ` | `+`/`-` lines get add/remove backgrounds |
| `ToolResult(bool)` | `  ` | header is `Result`; body is collapsed output |
| `ShellResult(bool)` | `  ` | last 100 lines of output |
| `Error` | `  ` | |
| `System` | `  ` | approvals, cancellations, compaction |
| `Separator` | `  ` | turn end: `Worked for 1.2s`, or a blank line when the
duration is unknown |

`User` and `Shell` span the full width inside their frames; every other kind is pushed right by a one-column
message indent, so all row bodies start at the same column. A collapsible row
draws its expand/collapse marker (`▸ ` / `▾ `) in that gutter slot instead of the
blank gutter, and its body lines render at the same column as any other row.

Collapsible rows auto-collapse when a new row arrives, unless the user expanded
them manually — `Collapsible` tracks that as `pinned`. Double-click (500 ms) on
a header toggles expansion; hover paints a background so headers read as
clickable.

A collapsible body that is still receiving content — live thinking deltas and
an open tool burst — renders only its newest `MAX_LIVE_BODY_ROWS` (8) screen
rows, counted after wrapping so one long line can spend the whole budget on its
own, behind a dim `… N earlier lines` marker that spends one of those rows. So a
growing row cannot scroll older rows out of the view. When the row stops
streaming it collapses at once (the thinking row as soon as its duration is
reported, the burst as soon as it closes) so no expanded body is left waiting
for the next row; once it stops streaming the full body renders again on expand.

Tool calls that declare `view.collapse` are grouped into one `ToolBurst` row
(`transcript/tools.rs`) whose title counts by kind — `Searched 3 patterns, Read
1 file, 1 failed` — with the individual call summaries as the collapsed body.
The row is upserted in place as calls finish, so a burst does not spam the log.
Non-collapsing tools get their own row, plus a `Result` row on failure.

Thinking content is never streamed to the screen. A live thinking row shows
`Thinking...` with a per-character shimmer, and is retitled to `Thought for 1.2s`
when the agent reports its duration, or to the bare `Thought` label if the turn
ends without one. Resuming a session replays the same rows from
`App::history_timed_shared()`, including reordering reasoning ahead of the answer
for providers that persist it after the text. Every question is a `User` row
there, because nothing is running yet to pin one. A restored turn ends with its
own `Separator` computed from the user prompt's timestamp to the answer's, so the
`Worked for 1.2s` line survives a resume; a message that ended in a tool call
has none, because the answer it led to is still to come.

### Subagents

Design and rationale: [`subagents.md`](./subagents.md).

The driver's conversation is not the only one on screen. Every `AgentEvent`
arrives with an `agent_id`; events from the driver (`AppState.agent_id`) feed
`Ui::transcript`, and every other agent's feed their own `Transcript` in
`Ui::views`, created on first event and dropped when the registry stops
listing them.

Three surfaces, in increasing order of commitment:

| Surface | Shows | Entered by |
| --- | --- | --- |
| the strip | one row per subagent — `◆ explore#1 · running 12.0s · 3 tools · label` — active ones first, newest finished next, `+N` when it is capped at three rows | always, while any subagent exists |
| the driver's transcript | the `task` call as an ordinary tool row, its report as the collapsible body | always |
| the viewer | one subagent's whole transcript, replacing the driver's, with the composer's row turned into a hint | `/agents <n>`, or clicking a strip row |

Opening a view builds it if it does not exist yet, seeded with the label the
task was spawned under, and fills in from the subagent's events as they arrive —
so a subagent that is still queued, or that has not spoken yet, still opens
something. `/agents` applies mid-turn for the same reason: a view command that
waited for the reply would be useless by the time it ran.

While the viewer is open it owns the keyboard: `↑↓`/`PgUp`/`PgDn` and the
mouse scroll it, `x` stops that subagent, `Ctrl-C` still quits, and `Esc` goes
back to the chat. That `Esc` is the one that acts on a single press —
`EscAction::acts_immediately` — because it throws nothing away (the transcript
is still there to reopen) and a screen that ignores the first `Esc` reads as one
you are stuck on. Every other `Esc` still waits for a confirmation. The row that
replaces the composer says so: `explore#1 · running · esc back to the chat ·
↑↓ scroll · x stop`. `state.agents` counts the ones still working, which is what keeps
the frame ticking so their clocks move while the driver is idle — `state.busy`,
the driver's own turn, stays false so the composer is still free to send.

`Esc` priority is therefore: pop a queued message → leave the viewer → cancel
the driver's turn → rewind. Cancelling a turn cancels the subagents it spawned;
one that outlives its turn is stopped with `/agents stop <n>` or `x`.

### Scrolling and selection

`top` is `None` to follow the tail and `Some(index)` to anchor. Streaming never
changes `top`; only PageUp/PageDown, the mouse wheel, and a new turn do. A new
turn appends its User or Shell row and snaps straight back to the tail. While
following, that same active row is projected as an internal sticky header; once
anchored in history it is rendered only at its logical scroll position.

The pinned band is the only part of the screen outside that model: it cannot be
reached by scrolling, and the wheel over it scrolls nothing, because the
transcript hit-tests its own band. It is only there while a turn runs, so an
idle screen scrolls as one region again.

Expanding or collapsing a row re-anchors `top` afterwards, so the clicked header
stays on its screen row instead of being scrolled away by the rewrap.

Drag-selecting with the mouse highlights by display width (`transcript/
selection.rs` slices spans at column boundaries, so double-width glyphs copy
correctly) and copies on release — via `arboard`, falling back to an OSC52 escape
sequence for terminals without clipboard access. The transcript hit-tests the
whole conversation area, including user messages; a drag that starts there still
owns the pointer while it runs, including a release outside the area.

### Quitting

`Ctrl-C` stops what is still running before the process goes away, in the order
the work was created: the driver's turn is cancelled, then the subagents it
spawned are stopped (`App::stop_subagents`), and `App::shutdown`
cancels the supervisor's root token as a backstop for anything that slipped
through. The exit is immediate — no frame is held for the stopped subagents,
because they are not persisted and the process is going away either way.

Anything the user queued and never sent is dropped, and said out loud on the
way out: the count is printed where the session hint goes, so it is readable in
the scrollback rather than in a frame that is already gone. It covers both
queues — the messages the composer still holds, and the ones already handed to
the runtime that are waiting behind the running turn (`App::shutdown` returns
that second count, since only the runtime knows it).

```text
dropped 2 queued messages (never sent)
oven -s 0192f0c1-…
```

A subagent's clock stops when it does: `NodeInfo::elapsed_ms` counts up while
it runs and freezes at `finished_at` once it is done, failed or cancelled, so a
stopped subagent on the strip shows the time it took rather than the time since
it started.

## Rendering notes

- `paint_visible` marks cells `AlwaysUpdate` for the transcript region, and the
  composer does the same for its text area: wide CJK glyphs leave a stale
  trailing cell under diff-based painting, which shows up as a white block
  when those characters are deleted.
- `draw` order in `Ui` is user prompt, transcript, queue, todos, input,
  overlay, status, then the reply toast above the transcript.
- Ticks are only scheduled while something animates (`wants_tick`), so an idle
  TUI does not wake up 12 times a second.

## Tests

Every component is unit-tested in-file, and rendering tests use
`ratatui::TestBackend` to assert on the real buffer (border glyphs, gutters,
`…` truncation, hover backgrounds) instead of mocking a frame. `transcript/
tests.rs` additionally asserts that resumed history renders the same row kinds
as the live event stream, and that collapsing, thinking clocks, and tool bursts
behave identically in both paths. No test relies on sleeps; animation is driven
by passing an explicit `frame` counter.

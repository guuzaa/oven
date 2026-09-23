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
delta. A disconnected channel clears `busy`.

`apply_event` maps each event onto four things:

- `Ui`'s own `state.busy` / `state.mode` / `rewinding` / `quit`
- transcript, status, input, and todos widgets via `on_event`
- overlay prompts (`OverlayPrompt`) for tool approval and loop limit
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
ignore. During a rewind the `rewinding` flag blocks a plain Enter until
`HistoryChanged` arrives, so the user cannot submit before the backend has
truncated history.

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

`components/layout.rs` splits the screen into named rows, bottom-up:

```text
┌──────────────────────────┐
│ transcript (Min 3 rows)  │
├──────────────────────────┤
│ queue       (0 or 1)     │
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

Each band is clamped against the remaining height, so the status bar and the
transcript minimum always survive a short terminal. Bands that collapse to zero
height are omitted entirely and drawn by nothing.

`components/terminal.rs` owns the raw-mode lifecycle: raw mode, alternate
screen, mouse capture, bracketed paste on `setup()`, and the inverse on
`restore()`. `components/theme.rs` is a flat list of style functions — one per
line kind, border state, and status segment.

## Components

| Module | Role |
| --- | --- |
| `input.rs` | multi-line composer; dispatches to the four overlays; dynamic height; border colour encodes mode |
| `status.rs` | bottom status row plus the transient reply toast |
| `transcript/` | scrolling conversation, streaming, selection, tool grouping |
| `setup_wizard.rs` | staged provider configuration |
| `model_picker.rs` | two-stage model + reasoning-effort picker |
| `slash_command_popup.rs` | `/` command completion |
| `file_mention_popup.rs` | `@` file completion |
| `choice_popup.rs` | the approve/reject and continue/exit modals |
| `queue.rs` | the queued-message row |
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
receives every key, so the composer line underneath stays frozen.

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

### Model picker

`/model` and `/model <fragment>` open a modal instead of submitting. Stage one
filters the model list (matching the slug or the provider wire id); Enter
promotes to stage two, which picks a reasoning effort and composes
`/model <id> <effort>`. `keep current` emits `/model <id>` with no effort. Two or
more arguments skip the picker and submit directly, which keeps a manual
fast path.

### Setup wizard

`/setup` with no arguments walks five stages: provider name (with `keep
current` and `custom`) → custom gateway id → base URL → protocol → api key. Text
stages echo what is typed; the api key stage shows `*` per character. `requires_
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

When the row does not fit, the hint is dropped and the left side is truncated
with `…` by display width.

App notifications become a toast: 3 s TTL, anchored bottom-right over the
transcript, and a 150 ms blank flash when a new reply replaces a visible one so
the change is noticeable without reading it.

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
| `User` | `› ` | |
| `Shell` | `$ ` | |
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

`User` alone sits flush left; every other kind is pushed right by a one-column
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
for providers that persist it after the text. A restored turn ends with its own
`Separator` computed from the user prompt's timestamp to the answer's, so the
`Worked for 1.2s` line survives a resume; a message that ended in a tool call
has none, because the answer it led to is still to come.

### Scrolling and selection

`top` is `None` to follow the tail and `Some(index)` to anchor. Streaming never
changes `top`; only PageUp/PageDown, the mouse wheel, and a new user message do.
A submitted message snaps straight back to the tail (`push_user` resets `top`),
so the reply keeps rendering on the bottom edge and extending downward.

Expanding or collapsing a row re-anchors `top` afterwards, so the clicked header
stays on its screen row instead of being scrolled away by the rewrap.

Drag-selecting with the mouse highlights by display width (`transcript/
selection.rs` slices spans at column boundaries, so double-width glyphs copy
correctly) and copies on release — via `arboard`, falling back to an OSC52 escape
sequence for terminals without clipboard access.

## Rendering notes

- `paint_visible` marks cells `AlwaysUpdate` for the transcript region: wide CJK
  glyphs leave a stale trailing cell on Windows terminals under diff-based
  painting.
- `draw` order in `Ui` is transcript, queue, todos, input, overlay, status, then
  the reply toast above the transcript.
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

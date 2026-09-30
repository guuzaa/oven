# Benchmarks

Scripts that drive the TUI from outside, the way a user does, and report what a
terminal sees: how much the app writes during a turn, and how long its keystrokes
take to come back. A TUI regression shows up in those two numbers long before it
shows up in `cargo test`.

Both scripts need a working provider in `~/.config/oven/config.toml`, since every
run makes real requests. Build the binary first (`cargo build`, or
`cargo build --release` and pass `--release`).

| Script | What it does |
| --- | --- |
| `smoke.py` | one turn: the answer reaches the screen, the wheel scrolls while it streams, `Ctrl-C` stops the process, and stderr stays empty. Exits non-zero on a failure. |
| `stream_latency.py` | one turn twice — once read as fast as it arrives, once drained at 20 kB/s like a terminal that cannot keep up — reporting bytes written, the peak write rate, the wheel reaction time and the `Ctrl-C` to exit time. |
| `oven_pty.py` | what they share: the pty, the keystrokes, and reading what the app wrote. Not meant to be run. |

## Run them

```sh
python3 benchmark/smoke.py
python3 benchmark/stream_latency.py            # or --release, --prompt TEXT
```

## Reading the numbers

`bytes written` is what the turn alone cost — startup and the frames the typed
prompt costs are drained before the turn starts. `peak write rate` is the
busiest 0.4 s window. `wheel, idle` and `ctrl-c to exit` are measured once the
answer is done, so they read the app's responsiveness rather than the model's
streaming.

What a healthy run looks like, measured on this repo (120×40, release):

| | before the frame coalescing | after |
| --- | --- | --- |
| bytes written, 40-line turn | ~380 kB | ~130 kB |
| peak write rate | ~700 kB/s | ~27 kB/s |
| wheel, idle | 50–80 ms | ~2 ms |

A turn is the only time the app writes more than a keypress is worth: a provider
streams tens of chunks per second, each one used to repaint the whole transcript.
Reading the numbers with a slow drain is what catches that, because the kernel
buffer filling up stops the loop from polling input at all.

## The transcript's per-event cost

The write rate says nothing about the CPU the event loop spends rewrapping the
conversation, which the row model's history made proportional to the whole
session. Measure it the same way every time — a `#[test]` in
`crates/oven-tui/src/widgets/transcript/tests.rs` that prints `Instant` deltas
around `on_event`, run with `cargo test --release -- --nocapture`:

```rust
let mut t = Transcript::new();
wide(&mut t);
t.area.height = 40;
for i in 0..1500 {
    let body: String = (0..12).map(|l| format!("line {l} of row {i} with some filler text here\n")).collect();
    t.push_row(LineKind::Text, &body);
}
ready(&mut t, Rect::new(0, 0, 80, 40));
t.on_event(&tool_start(1, "bash", serde_json::json!({ "command": "ls" })));
```

On 1500 rows (19.5k wrapped lines) a tool start/finish pair costs ~4 µs and a
thinking delta ~26 µs after the rows are rewrapped in place; before, when each
event rewrapped the whole transcript, both cost ~4 ms.

"""Throughput and input latency of a streamed turn.

A turn is the only time the app writes more than a keypress is worth, and the
only time input can be starved by it, so this measures both: how much the app
writes while an answer streams, and how long a keypress takes to come back once
the terminal it writes to cannot keep up.

Usage:
    python3 benchmark/stream_latency.py [--release] [--prompt TEXT]

Needs a working provider in ~/.config/oven/config.toml: every run makes two
real requests.
"""

import argparse
import shutil
import tempfile
import time

from oven_pty import IDLE_WAIT, Session, announce

SAMPLE_WINDOW = 0.4
TURN_WINDOW = 25.0
REACTION_TIMEOUT = 5.0
SLOW_CHUNK = 2048
SLOW_PAUSE = 0.1
LAST_LINE = "line 40"


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true", help="use target/release")
    parser.add_argument(
        "--prompt",
        default=f"Reply with exactly 40 short lines: '{LAST_LINE}' included.",
        help="the prompt to send",
    )
    return parser.parse_args()


def reaction(session, poke):
    """How long a wheel notch takes to come back as output."""
    started = time.monotonic()
    poke()
    while time.monotonic() - started < REACTION_TIMEOUT:
        if session.read() > 0:
            return round(time.monotonic() - started, 4)
    return None


def stream_turn(session, prompt, slow):
    """Runs one turn and measures the writes it costs and the keys it answers."""
    session.pump(IDLE_WAIT)
    session.type(prompt)
    session.pump(IDLE_WAIT)
    # Startup frames and the frames typing cost are not the turn's cost.
    startup = len(session.output)
    session.enter()

    rates = []
    deadline = time.monotonic() + TURN_WINDOW
    while time.monotonic() < deadline:
        before = len(session.output)
        window = time.monotonic()
        if slow:
            # A terminal that renders slower than the app writes.
            while time.monotonic() - window < SAMPLE_WINDOW:
                session.read(SLOW_CHUNK, SLOW_PAUSE)
        else:
            session.pump(SAMPLE_WINDOW)
        rates.append((len(session.output) - before) / SAMPLE_WINDOW)
        if LAST_LINE in session.text():
            break
    session.pump(IDLE_WAIT)

    idle_wheel = reaction(session, session.wheel_up)
    started = time.monotonic()
    session.ctrl_c()
    stopped = session.wait_for_exit(IDLE_WAIT)
    try:
        return {
            "bytes": len(session.output) - startup,
            "peak_rate": round(max(rates)) if rates else 0,
            "idle_wheel": idle_wheel,
            "ctrl_c": round(time.monotonic() - started, 4),
            "stopped": stopped,
        }
    finally:
        session.stop()


def report(name, result):
    announce(name)
    announce(f"  bytes written    {result['bytes']:>12,}")
    announce(f"  peak write rate  {result['peak_rate']:>12,} B/s")
    announce(f"  wheel, idle      {result['idle_wheel']:>12} s")
    announce(f"  ctrl-c to exit   {result['ctrl_c']:>12} s")
    announce(f"  process stopped  {str(result['stopped']):>12}")


def main():
    args = parse_args()
    workdir = tempfile.mkdtemp(prefix="oven-bench-")
    try:
        report(
            "fast terminal:",
            stream_turn(Session(workdir, args.release), args.prompt, slow=False),
        )
        report(
            "slow terminal:",
            stream_turn(Session(workdir, args.release), args.prompt, slow=True),
        )
    finally:
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    main()

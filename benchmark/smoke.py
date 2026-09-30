"""Smoke test: a real turn, streamed and read like a user would read it.

Checks what a TUI regression shows up as first: the answer reaches the screen,
the wheel still scrolls while it streams, the turn settles, and Ctrl-C leaves
the process cleanly. Run it after touching anything in `oven-tui`.

Usage:
    python3 benchmark/smoke.py [--release] [--prompt TEXT] [--timeout SECONDS]

Needs a working provider in ~/.config/oven/config.toml: every run makes one
real request.
"""

import argparse
import shutil
import tempfile
import time

from oven_pty import IDLE_WAIT, PANIC_MARKERS, Session, announce

WHEEL_STEP = 0.5
ANSWER_LINE = "line 1"
TAIL = 4096


WHEEL_STEP = 0.5
ANSWER_LINE = b"line 1"
TAIL = 4096
SETTLE = 2.0


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true", help="use target/release")
    parser.add_argument(
        "--prompt",
        default="Reply with exactly 20 short lines: 'line N' for N from 1 to 20.",
        help="the prompt to send",
    )
    parser.add_argument(
        "--timeout", type=float, default=120.0, help="how long to wait for the turn"
    )
    return parser.parse_args()


def wait_for(answer, session, deadline, on_tick):
    """Runs `on_tick` every wheel step until `answer` sees what it waits for."""
    while time.monotonic() < deadline:
        session.read(timeout=WHEEL_STEP)
        on_tick()
        if answer():
            return True
    return False


def run(args):
    workdir = tempfile.mkdtemp(prefix="oven-smoke-")
    try:
        session = Session(workdir, release=args.release)
        session.pump(IDLE_WAIT)
        announce(f"prompt: {args.prompt}")

        session.type(args.prompt)
        session.enter()
        deadline = time.monotonic() + args.timeout

        answered = wait_for(
            lambda: ANSWER_LINE in bytes(session.output[-TAIL:]),
            session,
            deadline,
            session.wheel_up,
        )
        settled = wait_for(
            lambda: session.read(timeout=SETTLE) == 0,
            session,
            deadline,
            lambda: session.pump(WHEEL_STEP),
        )

        session.ctrl_c()
        stopped = session.wait_for_exit(IDLE_WAIT)
        session.stop()
        errors = session.stderr()

        announce(f"answer reached the screen: {answered}")
        announce(f"the turn settled:          {settled}")
        announce(f"ctrl-c stopped the app:    {stopped}")
        announce(f"stderr: {errors.strip()[:400] or '(empty)'}")

        failures = []
        if not answered:
            failures.append("the answer never reached the screen")
        if not settled:
            failures.append("the turn never settled")
        if not stopped:
            failures.append("ctrl-c did not stop the app")
        for marker in PANIC_MARKERS:
            if marker in errors:
                failures.append(f"stderr mentions {marker!r}")
        return failures
    finally:
        shutil.rmtree(workdir, ignore_errors=True)


def main():
    failures = run(parse_args())
    for failure in failures:
        announce(f"FAIL: {failure}")
    announce("PASS" if not failures else f"{len(failures)} failure(s)")


if __name__ == "__main__":
    main()

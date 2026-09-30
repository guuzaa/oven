"""Driving the oven TUI through a pseudo-terminal: what the benchmark scripts
share. Every measure is taken from outside the process, so what a script reports
is what a user at a terminal sees."""

import fcntl
import os
import pty
import select
import struct
import subprocess
import termios
import time

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

ROWS = 40
COLS = 120
READ_CHUNK = 1 << 16
POLL = 0.02
IDLE_WAIT = 5.0

WHEEL_UP = b"\x1b[<65;20;20M"
ENTER = b"\r"
CTRL_C = b"\x03"

PANIC_MARKERS = (
    "panicked",
    "thread '",
    "RUST_BACKTRACE",
)


class Session:
    """One oven process in a pty, with everything it wrote kept in `output`."""

    def __init__(self, workdir, release=False):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        binary = os.path.join(
            REPO_ROOT, "target", "release" if release else "debug", "oven"
        )
        if not os.path.exists(binary):
            raise SystemExit(f"no binary at {binary}: run cargo build first")
        env = dict(os.environ, TERM="xterm-256color")
        self.proc = subprocess.Popen(
            [binary, "-C", workdir],
            stdin=slave,
            stdout=slave,
            stderr=subprocess.PIPE,
            env=env,
            cwd=workdir,
        )
        os.close(slave)
        self.output = bytearray()

    def read(self, limit=READ_CHUNK, timeout=POLL):
        """Reads at most `limit` bytes within `timeout`, and returns how many
        arrived: the size of one write a terminal would have to render."""
        ready, _, _ = select.select([self.master], [], [], timeout)
        if not ready:
            return 0
        try:
            data = os.read(self.master, limit)
        except OSError:
            return 0
        if not data:
            return 0
        self.output.extend(data)
        return len(data)

    def pump(self, timeout):
        """Reads for `timeout` seconds, and returns everything that arrived."""
        end = time.monotonic() + timeout
        added = 0
        while time.monotonic() < end:
            added += self.read()
        return added

    def wait_for_output(self, timeout):
        """Whether anything at all arrived within `timeout`."""
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if self.read() > 0:
                return True
        return False

    def write(self, data):
        os.write(self.master, data)

    def type(self, text, delay=0.003):
        for char in text:
            self.write(char.encode())
            time.sleep(delay)

    def enter(self):
        self.write(ENTER)

    def wheel_up(self):
        self.write(WHEEL_UP)

    def ctrl_c(self):
        self.write(CTRL_C)

    def text(self):
        return bytes(self.output).decode("utf-8", "replace")

    def alive(self):
        return self.proc.poll() is None

    def wait_for_exit(self, timeout):
        """Whether the process went away within `timeout`."""
        try:
            self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            return False
        return True

    def stderr(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            self.proc.wait(timeout=IDLE_WAIT)
        return self.proc.stderr.read().decode("utf-8", "replace")

    def stop(self):
        if self.proc.poll() is None:
            self.proc.kill()
        self.proc.wait(timeout=IDLE_WAIT)


def announce(message):
    print(message, flush=True)

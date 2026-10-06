"""Real-binary PTY session driver for the E2E suite."""

from __future__ import annotations

import errno
import fcntl
import os
import pty
import select
import signal
import struct
import subprocess
import termios
import threading
import time
from pathlib import Path
from typing import Callable, Sequence

from .vt import Screen


#: Named keys, encoded as an xterm-256color terminal would send them.
KEYS: dict[str, bytes] = {
    "enter": b"\r",
    "escape": b"\x1b",
    "tab": b"\t",
    "shift_tab": b"\x1b[Z",
    "backspace": b"\x7f",
    "up": b"\x1b[A",
    "down": b"\x1b[B",
    "right": b"\x1b[C",
    "left": b"\x1b[D",
    "home": b"\x1b[H",
    "end": b"\x1b[F",
    "page_up": b"\x1b[5~",
    "page_down": b"\x1b[6~",
    "insert": b"\x1b[2~",
    "delete": b"\x1b[3~",
    "ctrl_a": b"\x01",
    "ctrl_b": b"\x02",
    "ctrl_c": b"\x03",
    "ctrl_d": b"\x04",
    "ctrl_e": b"\x05",
    "ctrl_g": b"\x07",
    "ctrl_k": b"\x0b",
    "ctrl_l": b"\x0c",
    "ctrl_o": b"\x0f",
    "ctrl_p": b"\x10",
    "ctrl_q": b"\x11",
    "ctrl_r": b"\x12",
    "ctrl_s": b"\x13",
    "ctrl_t": b"\x14",
    "ctrl_u": b"\x15",
    "ctrl_v": b"\x16",
    "ctrl_w": b"\x17",
    "ctrl_x": b"\x18",
    "ctrl_y": b"\x19",
    "ctrl_z": b"\x1a",
    "shift_page_up": b"\x1b[5;2~",
    "shift_page_down": b"\x1b[6;2~",
}


class CheckFailure(AssertionError):
    """A check's assertion failed with a user-visible explanation."""


class OctetSession:
    """One octet process on a disposable PTY with a VT screen model."""

    def __init__(
        self,
        binary: Path,
        *,
        args: Sequence[str],
        env: dict[str, str],
        cwd: Path,
        columns: int = 120,
        rows: int = 40,
        log_path: Path | None = None,
    ):
        self.binary = Path(binary)
        self.args = list(args)
        self.env = dict(env)
        self.cwd = Path(cwd)
        self.columns, self.rows = columns, rows
        self.log_path = log_path
        self._lock = threading.Lock()
        self._screen = Screen(columns, rows)
        self._transcript = bytearray()
        self._reader: threading.Thread | None = None
        self._closed = False
        self.started_at = 0.0

        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        self.master, self.slave = master, slave
        self.process = subprocess.Popen(
            [str(self.binary), *self.args],
            cwd=str(self.cwd),
            env=self.env,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            start_new_session=True,
            close_fds=True,
        )
        os.close(slave)
        os.set_blocking(master, False)
        self.started_at = time.monotonic()
        self._reader = threading.Thread(target=self._read_loop, name="octet-pty-reader", daemon=True)
        self._reader.start()

    # ------------------------------------------------------------- lifecycle
    def _read_loop(self) -> None:
        while True:
            try:
                ready, _, _ = select.select([self.master], [], [], 0.05)
            except (OSError, ValueError):
                return
            if not ready:
                if self.process.poll() is not None:
                    self._drain()
                    return
                continue
            try:
                data = os.read(self.master, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    return
                raise
            if not data:
                return
            with self._lock:
                self._transcript.extend(data)
                self._screen.feed(data)
                pending = self._screen.responses
                self._screen.responses = []
            for response in pending:
                self.write(response)

    def _drain(self) -> None:
        try:
            while True:
                data = os.read(self.master, 65536)
                if not data:
                    return
                with self._lock:
                    self._transcript.extend(data)
                    self._screen.feed(data)
        except OSError:
            return

    def close(self, *, ctrl_d: bool = True, timeout: float = 10.0) -> int:
        """Exit the TUI with Ctrl+D and reap it; returns the exit code."""
        if self.process.poll() is None and ctrl_d:
            try:
                self.write(KEYS["ctrl_d"])
            except OSError:
                pass
        deadline = time.monotonic() + timeout
        while self.process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.02)
        if self.process.poll() is None:
            self.kill()
        self._closed = True
        try:
            os.close(self.master)
        except OSError:
            pass
        if self._reader is not None:
            self._reader.join(timeout=1.0)
        if self.log_path is not None:
            self.log_path.write_bytes(bytes(self._transcript))
        return self.process.returncode if self.process.returncode is not None else -1

    def kill(self) -> None:
        if self.process.poll() is None:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.process.wait(timeout=3)
        if self.log_path is not None and not self.log_path.exists():
            self.log_path.write_bytes(bytes(self._transcript))
        try:
            os.close(self.master)
        except OSError:
            pass
        self._closed = True

    def __enter__(self) -> "OctetSession":
        return self

    def __exit__(self, *_exc) -> None:
        self.close()

    # ----------------------------------------------------------------- input
    def write(self, data: bytes | str) -> None:
        payload = data.encode() if isinstance(data, str) else data
        os.write(self.master, payload)

    def type_text(self, text: str) -> None:
        self.write(text)

    def key(self, name: str) -> None:
        try:
            self.write(KEYS[name])
        except KeyError as error:
            raise ValueError(f"unknown key {name!r}") from error

    def submit(self, text: str) -> None:
        self.type_text(text)
        self.key("enter")

    def clear_composer(self, text: str) -> None:
        for _ in range(len(text)):
            self.key("backspace")

    def mouse(self, x: int, y: int, *, kind: str = "press", button: int = 0, motion: bool = False) -> None:
        """SGR mouse event; x/y are zero-based screen cells."""
        code = button
        if motion:
            code += 32
        final = "m" if kind == "release" else "M"
        self.write(f"\x1b[<{code};{x + 1};{y + 1}{final}")

    def drag(self, x0: int, y0: int, x1: int, y1: int) -> None:
        self.mouse(x0, y0, kind="press")
        time.sleep(0.05)
        self.mouse(x1, y1, kind="press", motion=True)
        time.sleep(0.05)
        self.mouse(x1, y1, kind="release")

    def resize(self, columns: int, rows: int) -> None:
        self.columns, self.rows = columns, rows
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        os.kill(self.process.pid, signal.SIGWINCH)
        with self._lock:
            self._screen.resize(columns, rows)

    # -------------------------------------------------------------- observe
    def screen_text(self) -> str:
        with self._lock:
            return self._screen.text()

    def screen(self) -> Screen:
        with self._lock:
            snapshot = Screen(self.columns, self.rows)
            snapshot.grid = [row[:] for row in self._screen.grid]
            snapshot.styles = [row[:] for row in self._screen.styles]
            snapshot.scrollback = list(self._screen.scrollback)
            snapshot.osc52 = list(self._screen.osc52)
            snapshot.bracketed_paste = self._screen.bracketed_paste
            snapshot._visible = self._screen._visible
            return snapshot

    def osc52(self) -> list[str]:
        with self._lock:
            return list(self._screen.osc52)

    def transcript(self) -> bytes:
        with self._lock:
            return bytes(self._transcript)

    def transcript_text(self) -> str:
        """Control sequences stripped; for grepping raw diagnostics."""
        import re

        text = self.transcript().decode("utf-8", "replace")
        text = re.sub(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)", "", text)
        text = re.sub(r"\x1b\[[0-9;?<=>]*[A-Za-z@~]", "", text)
        return text

    def wait_for(self, predicate: Callable[[Screen], bool], *, timeout: float = 30.0, label: str = "condition") -> None:
        deadline = time.monotonic() + timeout
        last = ""
        while time.monotonic() < deadline:
            snapshot = self.screen()
            if predicate(snapshot):
                return
            last = snapshot.text()
            if self.process.poll() is not None:
                raise CheckFailure(
                    f"octet exited with {self.process.returncode} while waiting for {label}\n{tail(last)}"
                )
            time.sleep(0.02)
        raise CheckFailure(f"timed out after {timeout}s waiting for {label}\n{tail(last)}")

    def wait_for_text(self, needle: str, *, timeout: float = 30.0) -> None:
        self.wait_for(lambda screen: needle in screen.text(), timeout=timeout, label=f"text {needle!r}")

    def wait_for_gone(self, needle: str, *, timeout: float = 30.0) -> None:
        self.wait_for(lambda screen: needle not in screen.text(), timeout=timeout, label=f"{needle!r} to leave the screen")

    def wait_ready(self, model: str, *, timeout: float = 30.0) -> None:
        self.wait_for_text(model, timeout=timeout)

    def wait_working(self, *, timeout: float = 30.0) -> None:
        self.wait_for_text("Working", timeout=timeout)

    def wait_idle(self, *, timeout: float = 60.0) -> None:
        self.wait_for(lambda screen: "Working" not in screen.text(), timeout=timeout, label="the turn to settle")

    def wait_exit(self, *, timeout: float = 15.0) -> int:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                return self.process.returncode
            time.sleep(0.02)
        raise CheckFailure(f"octet did not exit within {timeout}s")

    @property
    def exit_code(self) -> int | None:
        return self.process.returncode

    def tail(self, lines: int = 12) -> str:
        return tail(self.screen_text(), lines=lines)


def tail(text: str, lines: int = 12) -> str:
    kept = [line for line in text.splitlines() if line.strip()][-lines:]
    return "\n".join(kept)

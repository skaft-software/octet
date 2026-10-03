#!/usr/bin/env python3
"""Real-binary Pi UI acceptance and optional local Pi startup comparison.

No real credentials or inference. Each process gets a disposable HOME/workspace.
Source entrypoints are loaded unchanged; downloads/installation are never implicit.
Captures are bounded in memory and optional saved artifacts are synthetic-only.
"""
from __future__ import annotations

import argparse
import codecs
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata

ROOT = Path(__file__).resolve().parents[1]
CAPTURE_LIMIT = 8 * 1024 * 1024


class Screen:
    """Small VT observer for the cursor/erase/SGR subset emitted by these UIs."""
    def __init__(self, columns=96, rows=32):
        self.columns, self.rows = columns, rows
        self.grid = [[" "] * columns for _ in range(rows)]
        self.x = self.y = 0
        self.saved = (0, 0)
        self.state = "text"
        self.sequence = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.frames = 0
        self.rgb = 0

    def resize(self, columns, rows):
        self.columns, self.rows = columns, rows
        self.grid = [row[:columns] + [" "] * max(0, columns-len(row))
                     for row in self.grid[:rows]]
        self.grid += [[" "] * columns for _ in range(rows-len(self.grid))]
        self.x, self.y = min(self.x, columns-1), min(self.y, rows-1)

    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.grid)

    def csi(self, final):
        body = self.sequence
        nums = [int(n) if n else 0 for n in body.lstrip("?<>=").split(";")
                if n.isdigit() or not n]
        n = (nums or [0])[0]
        count = n or 1
        if final in "Hf":
            self.y = min(self.rows-1, max(0, count-1))
            self.x = min(self.columns-1, max(0, (nums[1] or 1)-1)) if len(nums)>1 else 0
        elif final == "A": self.y = max(0, self.y-count)
        elif final == "B": self.y = min(self.rows-1, self.y+count)
        elif final == "C": self.x = min(self.columns-1, self.x+count)
        elif final == "D": self.x = max(0, self.x-count)
        elif final == "G": self.x = min(self.columns-1, count-1)
        elif final == "d": self.y = min(self.rows-1, count-1)
        elif final == "J":
            if n in (2, 3): self.grid = [[" "] * self.columns for _ in range(self.rows)]
            elif n == 0:
                self.grid[self.y][self.x:] = [" "] * (self.columns-self.x)
                for y in range(self.y+1, self.rows): self.grid[y] = [" "] * self.columns
        elif final == "K":
            if n == 2: self.grid[self.y] = [" "] * self.columns
            elif n == 0: self.grid[self.y][self.x:] = [" "] * (self.columns-self.x)
            elif n == 1: self.grid[self.y][:self.x+1] = [" "] * (self.x+1)
        elif final == "m":
            self.rgb += body.count("38;2;") + body.count("48;2;")
        elif final == "l" and body == "?2026": self.frames += 1
        elif final == "s": self.saved = (self.x, self.y)
        elif final == "u": self.x, self.y = self.saved

    def feed(self, data):
        for char in self.decoder.decode(data):
            if self.state == "osc":
                if char == "\a": self.state = "text"
                elif char == "\x1b": self.state = "osc_escape"
                continue
            if self.state == "osc_escape":
                self.state = "text" if char == "\\" else "osc"
                continue
            if self.state == "escape":
                if char == "[": self.state, self.sequence = "csi", ""
                elif char in "]P_": self.state = "osc"
                elif char == "7": self.saved, self.state = (self.x, self.y), "text"
                elif char == "8": (self.x, self.y), self.state = self.saved, "text"
                else: self.state = "text"
                continue
            if self.state == "csi":
                if "@" <= char <= "~": self.csi(char); self.state = "text"
                else: self.sequence += char
                continue
            if char == "\x1b": self.state = "escape"
            elif char == "\r": self.x = 0
            elif char == "\n":
                self.y += 1
                if self.y >= self.rows:
                    self.grid.pop(0); self.grid.append([" "] * self.columns); self.y = self.rows-1
            elif char == "\b": self.x = max(0, self.x-1)
            elif char == "\t": self.x = min(self.columns-1, (self.x//8+1)*8)
            elif ord(char) >= 32 and char != "\x7f":
                if unicodedata.combining(char): continue
                width = 2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
                if self.x >= self.columns:
                    self.x = 0; self.y += 1
                    if self.y >= self.rows:
                        self.grid.pop(0); self.grid.append([" "] * self.columns); self.y = self.rows-1
                self.grid[self.y][self.x] = char
                if width == 2 and self.x+1 < self.columns: self.grid[self.y][self.x+1] = ""
                self.x += width


class TerminalProcess:
    def __init__(self, command, environment, workspace, columns=96, rows=32):
        self.master, self.slave = pty.openpty()
        self.original_termios = termios.tcgetattr(self.slave)
        self.screen = Screen(columns, rows)
        self.capture = bytearray()
        self.started = time.perf_counter()
        self.first_frame_ms = None
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        # Match the existing native terminal-handoff harness: stdin is the PTY,
        # but no session leader claims it as a controlling terminal. macOS
        # otherwise revokes the slave on exit, preventing restoration checks.
        self.process = subprocess.Popen(command, cwd=workspace, env=environment,
                                        stdin=self.slave, stdout=self.slave, stderr=self.slave,
                                        start_new_session=True, close_fds=True)
        os.set_blocking(self.master, False)

    def pump(self, seconds=0.05):
        deadline = time.perf_counter()+seconds
        while time.perf_counter() < deadline:
            ready, _, _ = select.select([self.master], [], [], max(0, deadline-time.perf_counter()))
            if not ready: break
            try: data = os.read(self.master, 65536)
            except OSError as error:
                if error.errno == errno.EIO: break
                raise
            if not data: break
            if len(self.capture)+len(data) > CAPTURE_LIMIT:
                raise AssertionError("synthetic PTY capture exceeded 8 MiB bound")
            self.capture.extend(data)
            self.screen.feed(data)
            if self.first_frame_ms is None and self.screen.frames:
                self.first_frame_ms = (time.perf_counter()-self.started)*1000

    def until(self, predicate, timeout=20, label="screen condition"):
        deadline = time.perf_counter()+timeout
        while time.perf_counter() < deadline:
            self.pump(0.025)
            if predicate(self.screen): return
            if self.process.poll() is not None:
                raise AssertionError(f"process exited {self.process.returncode} waiting for {label}\n{self.screen.text()}")
        raise AssertionError(f"timeout waiting for {label}\n{self.screen.text()}")

    def send(self, data): os.write(self.master, data)

    def resize(self, columns, rows):
        self.screen.resize(columns, rows)
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        os.kill(self.process.pid, signal.SIGWINCH)

    def close(self):
        if self.process.poll() is None:
            self.send(b"\x04")
            end = time.perf_counter()+5
            while self.process.poll() is None and time.perf_counter() < end: self.pump(0.025)
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)
            self.process.wait(timeout=5)
            raise AssertionError("Ctrl+D did not complete coordinated shutdown")
        self.pump(0.05)
        if self.process.returncode != 0: raise AssertionError(f"exit={self.process.returncode}\n{self.screen.text()}")
        restored = termios.tcgetattr(self.slave)
        if restored != self.original_termios: raise AssertionError("terminal termios was not restored")
        if b"\x1b[?2004l" not in self.capture: raise AssertionError("bracketed paste was not restored")

    def dispose(self):
        try:
            if self.process.poll() is None:
                self.process.terminate()
                try: self.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.process.kill(); self.process.wait(timeout=5)
        finally:
            os.close(self.master); os.close(self.slave)


def isolated(root):
    home, workspace = root/"home", root/"pi-ui-workspace"
    workspace.mkdir(); (home/".octet/credentials").mkdir(parents=True)
    credentials = home/".octet/credentials/custom.json"
    credentials.write_text(json.dumps({"base_url":"http://127.0.0.1:9/v1/", "api_key":"", "api_name":"probe",
                                       "headers":[], "models":[], "auto_discover":False}))
    credentials.chmod(0o600)
    pi_dir = home/".pi/agent"; pi_dir.mkdir(parents=True)
    (pi_dir/"models.json").write_text(json.dumps({"providers":{"probe":{"baseUrl":"http://127.0.0.1:9/v1/",
        "api":"openai-completions", "apiKey":"synthetic-pty-key", "models":[{"id":"probe", "name":"Probe",
        "reasoning":False, "input":["text"], "contextWindow":128000, "maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}]}}}))
    environment = {"HOME":str(home), "USERPROFILE":str(home), "PATH":os.defpath,
                   "TERM":"xterm-256color", "COLORTERM":"truecolor", "LANG":"C.UTF-8",
                   "OCTET_COLOR_SCHEME":"dark", "PI_OFFLINE":"1", "PI_CODING_AGENT_DIR":str(pi_dir)}
    return environment, workspace


def configure(adapter, node, root, entries, environment, workspace):
    bundle = root/"extensions/octet-pi-compat"
    subprocess.run([str(node), str(adapter/"configure.mjs"), "--reviewed", "--output", str(bundle), *map(str, entries)],
                   check=True, timeout=30, capture_output=True, env=environment, cwd=workspace)
    return root/"extensions"


def native_command(binary, extension_root=None):
    command = [str(binary), "--offline", "--model", "custom/probe", "--no-context-files", "--no-tools"]
    if extension_root:
        command += ["--extension-dir", str(extension_root), "--enable-extension", "octet-pi-compat",
                    "--trust-extension", "octet-pi-compat", "--allow-shell", "--effect-policy", "unsafe_host"]
    return command


def run_acceptance(args, entries):
    with tempfile.TemporaryDirectory(prefix="octet-pi-pty-") as name:
        root = Path(name).resolve(); environment, workspace = isolated(root)
        extension_root = configure(args.adapter, args.node, root, entries, environment, workspace)
        terminal = TerminalProcess(native_command(args.binary, extension_root), environment, workspace)
        report = {"entrypoints":[{"path":str(p), "sha256":hashlib.sha256(p.read_bytes()).hexdigest()} for p in entries]}
        try:
            terminal.until(lambda s: "probe" in s.text(), label="native ready model")
            if args.footer in entries:
                terminal.until(lambda s: bool(re.search(r"\d\d:\d\d", s.text())) and "pi-ui-workspace" in s.text(),
                               label="installed powerline cwd/clock footer")
                if b"\x1b[38;5;111m" not in terminal.capture: raise AssertionError("original footer indexed color missing")
                report["footer"] = "pass"
            if args.doom in entries:
                start = time.perf_counter(); terminal.send(b"/doom\r")
                terminal.until(lambda s: "DOOM | Q=Pause" in s.text() and "▀" in s.text(), label="original Doom RGB frame")
                report["doom_open_ms"] = round((time.perf_counter()-start)*1000, 1)
                # Doom's first framebuffer is black and its title is static.
                # Wait for real colored pixels, then start a game before timing
                # terminal repaints (the native diff renderer skips no-op frames).
                initial_rgb = terminal.screen.rgb
                terminal.until(lambda s: s.rgb > initial_rgb+500, label="Doom populated framebuffer")
                terminal.send(b"\x1b"); terminal.pump(0.4)
                terminal.send(b"\x1b[27;1:3u"); terminal.pump(0.1)
                for _ in range(3):
                    terminal.send(b"\r\x1b[13;1:3u"); terminal.pump(0.4)
                terminal.pump(1)  # finish the game's level-loading wipe
                # A stationary scene also legitimately produces no-op renders.
                # Hold a turn key, then release the same CSI direction key.
                terminal.send(b"\x1b[C"); terminal.pump(0.2)
                frames = terminal.screen.frames; terminal.pump(1)
                report["doom_frames_in_one_second"] = terminal.screen.frames-frames
                if report["doom_frames_in_one_second"] < 5: raise AssertionError("Doom gameplay did not repaint")
                terminal.send(b"\x1b[1;1:3C"); terminal.pump(0.2)
                frames = terminal.screen.frames; terminal.pump(1)
                report["doom_frames_after_key_release"] = terminal.screen.frames-frames
                if report["doom_frames_after_key_release"] >= report["doom_frames_in_one_second"]//2:
                    raise AssertionError("Doom camera did not stop after key release")
                terminal.resize(80, 28)
                terminal.until(lambda s: "DOOM | Q=Pause" in s.text(), label="Doom resized")
                start = time.perf_counter(); terminal.send(b"q")
                terminal.until(lambda s: "DOOM | Q=Pause" not in s.text(), label="Doom pause/close")
                report["doom_close_ms"] = round((time.perf_counter()-start)*1000, 1)
                terminal.send(b"/doom\r")
                terminal.until(lambda s: "DOOM | Q=Pause" in s.text(), label="Doom resume")
                terminal.send(b"\x07")
                terminal.until(lambda s: "DOOM | Q=Pause" not in s.text(), label="host Ctrl+G rescue")
                report["doom"] = "pass"
            if args.draw in entries:
                terminal.resize(160, 38)
                terminal.pump(0.15)
                terminal.send(b"/draw\r")
                terminal.until(lambda s: "mode:" in s.text() and "Enter save" in s.text(), label="original drawing modal")
                if b"\x1b[?1006h" not in terminal.capture: raise AssertionError("host-owned SGR mouse capture missing")
                terminal.send(b" ")
                terminal.until(lambda s: 'Stamped "#"' in s.text(), label="drawing keyboard input")
                terminal.send(b"\x1b[<0;5;7M\x1b[<32;14;7M\x1b[<0;14;7m")
                terminal.pump(0.1)
                terminal.send(b"\r")
                terminal.until(lambda s: "```text" in s.text() and "#" in s.text(), label="drawing inserted into native editor")
                terminal.send(b"\x03")
                terminal.pump(0.1)
                terminal.send(b"/draw\r")
                terminal.until(lambda s: "mode:" in s.text() and "Enter save" in s.text(), label="second drawing modal")
                terminal.send(b"\x1b")
                terminal.until(lambda s: "Drawing cancelled." in s.text(), label="drawing cancelled")
                report["draw"] = "pass"
            if args.editor in entries:
                previous = terminal.screen.rgb
                terminal.send(b"ultrathink")
                terminal.until(lambda s: "ultrathink" in s.text() and s.rgb > previous+10, label="original animated rainbow editor")
                frame = terminal.screen.frames; terminal.pump(0.3)
                if terminal.screen.frames <= frame+1: raise AssertionError("rainbow shine did not animate")
                terminal.resize(72, 24); terminal.pump(0.2)
                terminal.send(b"\x07")
                terminal.until(lambda s: "ultrathink" in s.text(), label="native draft restored after editor rescue")
                report["editor"] = "pass"
            terminal.send(b"\x03")
            terminal.pump(0.1)
            terminal.close()
            report.update({"first_frame_ms":round(terminal.first_frame_ms, 1), "shutdown":"pass", "captured_bytes":len(terminal.capture)})
            return report
        finally:
            try:
                if args.capture:
                    args.capture.parent.mkdir(parents=True, exist_ok=True)
                    args.capture.write_bytes(terminal.capture); args.capture.chmod(0o600)
            finally: terminal.dispose()


def startup_samples(args, command, pi=False):
    samples = []
    for _ in range(args.samples):
        with tempfile.TemporaryDirectory(prefix="octet-pi-startup-") as name:
            environment, workspace = isolated(Path(name).resolve())
            terminal = TerminalProcess(command, environment, workspace)
            try:
                terminal.until(lambda s: s.frames > 0, label="first complete synchronized frame")
                samples.append(round(terminal.first_frame_ms, 2))
                terminal.close()
            finally: terminal.dispose()
    return {"samples_ms":samples, "median_ms":round(statistics.median(samples), 2), "metric":"process spawn to first complete synchronized terminal frame"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--adapter", type=Path, default=ROOT/"extensions/octet-pi-compat")
    parser.add_argument("--node", type=Path, default=Path(shutil.which("node") or "/missing-node"))
    parser.add_argument("--doom", type=Path)
    parser.add_argument("--draw", type=Path)
    parser.add_argument("--footer", type=Path)
    parser.add_argument("--editor", type=Path)
    parser.add_argument("--pi-cli", type=Path, help="installed Pi dist/cli.js; isolated comparison only")
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--capture", type=Path, help="optional private synthetic-only capture (max8MiB)")
    args = parser.parse_args()
    if os.name != "posix": parser.error("Unix PTY required")
    if args.samples < 1 or args.samples > 20: parser.error("samples must be 1..20")
    args.binary, args.adapter, args.node = args.binary.resolve(), args.adapter.resolve(), args.node.resolve()
    for key in ("doom", "draw", "footer", "editor"):
        if getattr(args, key): setattr(args, key, getattr(args, key).resolve(strict=True))
    entries = [p for p in (args.doom, args.draw, args.footer, args.editor) if p]
    # Editor replacement is qualified separately from other components to avoid
    # the same typed slash commands being routed through the foreign editor.
    if args.editor and len(entries) > 1:
        parser.error("qualify --editor separately from Doom/draw/footer")
    report = {}
    if entries: report["acceptance"] = run_acceptance(args, entries)
    if args.pi_cli:
        pi_command = [str(args.node), str(args.pi_cli.resolve()), "--offline", "--provider", "probe", "--model", "probe",
                      "--no-session", "--no-extensions", "--no-skills", "--no-prompt-templates", "--no-context-files", "--no-themes", "--tui-mode", "regular"]
        report["startup"] = {"octet":startup_samples(args, native_command(args.binary)), "pi":startup_samples(args, pi_command, True)}
    if not report: parser.error("supply a UI entrypoint and/or --pi-cli")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    try: main()
    except (AssertionError, OSError, subprocess.SubprocessError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr.decode(errors="replace"), file=sys.stderr)
        sys.exit(1)

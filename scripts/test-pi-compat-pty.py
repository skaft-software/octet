#!/usr/bin/env python3
"""Real-binary Pi UI acceptance and optional local Pi startup comparison.

No real credentials or inference. Each process gets a disposable HOME/workspace.
Source entrypoints are loaded unchanged; downloads/installation are never implicit.
Captures are bounded, private PTY artifacts, not evidence of physical display FPS.
--doom-performance measures actual binary output without parsing VT in timed windows.
No provider is called; a supplied original factory and its assets execute unchanged.
"""
from __future__ import annotations

import argparse
import codecs
import errno
import fcntl
import hashlib
import json
import math
import platform
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
PERFORMANCE_LIMIT = 64 * 1024 * 1024
SYNC_END = b"\x1b[?2026l"


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def distribution(values):
    """Nearest-rank percentiles; retain raw samples separately for reproducibility."""
    ordered = sorted(values)
    if not ordered:
        return {"count": 0}
    return {"count": len(ordered), "min": min(ordered), "max": max(ordered),
            "mean": statistics.mean(ordered),
            **{f"p{p}": ordered[math.ceil(len(ordered)*p/100)-1] for p in (50, 95, 99)}}


class FrameClock:
    """Complete synchronized-output delimiters timestamped at PTY read, not paint.

    Fragmentation is handled; frames coalesced into one read share a timestamp.
    Zero intervals are reported, never converted into fabricated FPS.
    """
    def __init__(self):
        self.tail = b""
        self.bytes = 0
        self.frames = []
        self.reads = []

    def feed(self, data, now_ns):
        if len(self.reads) >= 100000 or len(self.frames) >= 10000:
            raise AssertionError("bounded PTY timing metadata exceeded")
        start = self.bytes - len(self.tail)
        joined = self.tail + data
        at = 0
        while (at := joined.find(SYNC_END, at)) >= 0:
            self.frames.append({"ns": now_ns, "end": start + at + len(SYNC_END)})
            at += len(SYNC_END)
        self.bytes += len(data)
        self.reads.append({"ns": now_ns, "end": self.bytes})
        self.tail = joined[-(len(SYNC_END)-1):]


def frame_window(clock, start_ns, end_ns):
    frames = [f for f in clock.frames if start_ns <= f["ns"] < end_ns]
    intervals = [(b["ns"]-a["ns"])/1e6 for a, b in zip(frames, frames[1:])]
    seconds = (end_ns-start_ns)/1e9
    return {"start_ns": start_ns, "end_ns": end_ns, "seconds": seconds,
            "synchronized_output_frames": len(frames), "output_fps": len(frames)/seconds,
            "interval_ms": distribution(intervals), "interval_samples_ms": intervals,
            "over_35fps_budget": sum(v > 1000/35 for v in intervals),
            "over_two_frame_budgets": sum(v > 2000/35 for v in intervals),
            "coalesced_read_intervals": intervals.count(0)}


class Screen:
    """Small VT observer for the cursor/erase/SGR subset emitted by these UIs."""
    def __init__(self, columns=96, rows=32, track_colors=False):
        self.columns, self.rows = columns, rows
        self.grid = [[" "] * columns for _ in range(rows)]
        self.x = self.y = 0
        self.saved = (0, 0)
        self.state = "text"
        self.sequence = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.frames = 0
        self.rgb = 0
        self.fg = self.bg = None
        self.colors = [[None] * columns for _ in range(rows)] if track_colors else None

    def resize(self, columns, rows):
        self.columns, self.rows = columns, rows
        self.grid = [row[:columns] + [" "] * max(0, columns-len(row))
                     for row in self.grid[:rows]]
        self.grid += [[" "] * columns for _ in range(rows-len(self.grid))]
        if self.colors is not None:
            self.colors = [row[:columns] + [None] * max(0, columns-len(row))
                           for row in self.colors[:rows]]
            self.colors += [[None] * columns for _ in range(rows-len(self.colors))]
        self.x, self.y = min(self.x, columns-1), min(self.y, rows-1)

    def scroll(self):
        self.grid.pop(0); self.grid.append([" "] * self.columns)
        if self.colors is not None:
            self.colors.pop(0); self.colors.append([None] * self.columns)
        self.y = self.rows-1

    def pixel_signature(self):
        """First three complete RGB half-block rows, for offline correlation only."""
        rows = []
        for glyphs, colors in zip(self.grid, self.colors or []):
            if all(c == "▀" for c in glyphs) and all(c and c[0] and c[1] for c in colors):
                rows.append(bytes(v for pair in colors for rgb in pair for v in rgb))
                if len(rows) == 3:
                    return hashlib.sha256(b"".join(rows)).hexdigest()
        return None

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
            if self.colors is not None:
                i = 0
                while i < len(nums):
                    code = nums[i]
                    if code == 0: self.fg = self.bg = None
                    elif code == 39 or 30 <= code <= 37 or 90 <= code <= 97: self.fg = None
                    elif code == 49 or 40 <= code <= 47 or 100 <= code <= 107: self.bg = None
                    elif code in (38, 48) and i+1 < len(nums):
                        count = 3 if nums[i+1] == 2 else 1
                        color = tuple(nums[i+2:i+5]) if count == 3 and i+4 < len(nums) else None
                        if code == 38: self.fg = color
                        else: self.bg = color
                        i += count+1
                    i += 1
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
                if self.y >= self.rows: self.scroll()
            elif char == "\b": self.x = max(0, self.x-1)
            elif char == "\t": self.x = min(self.columns-1, (self.x//8+1)*8)
            elif ord(char) >= 32 and char != "\x7f":
                if unicodedata.combining(char): continue
                width = 2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
                if self.x >= self.columns:
                    self.x = 0; self.y += 1
                    if self.y >= self.rows: self.scroll()
                self.grid[self.y][self.x] = char
                if self.colors is not None: self.colors[self.y][self.x] = (self.fg, self.bg)
                if width == 2 and self.x+1 < self.columns: self.grid[self.y][self.x+1] = ""
                self.x += width


class TerminalProcess:
    def __init__(self, command, environment, workspace, columns=96, rows=32,
                 capture_limit=CAPTURE_LIMIT):
        self.master, self.slave = pty.openpty()
        self.original_termios = termios.tcgetattr(self.slave)
        self.screen = Screen(columns, rows)
        self.capture = bytearray()
        self.capture_limit = capture_limit
        self.clock = FrameClock()
        self.screen_offset = 0
        self.defer_screen = False
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
            now_ns = time.monotonic_ns()
            if len(self.capture)+len(data) > self.capture_limit:
                raise AssertionError(f"PTY capture exceeded {self.capture_limit} byte bound")
            self.clock.feed(data, now_ns)
            self.capture.extend(data)
            if not self.defer_screen:
                self.observe()
            if self.first_frame_ms is None and self.clock.frames:
                self.first_frame_ms = (time.perf_counter()-self.started)*1000

    def observe(self):
        self.screen.feed(self.capture[self.screen_offset:])
        self.screen_offset = len(self.capture)

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
                os.killpg(self.process.pid, signal.SIGTERM)
                try: self.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(self.process.pid, signal.SIGKILL); self.process.wait(timeout=5)
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
                report["doom_open_ms"] = round(open_native_doom(terminal), 1)
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
                frames = terminal.screen.frames
                terminal.resize(80, 28)
                terminal.until(lambda s: s.frames > frames and "▀" in s.text() and "Ctrl+G return" in s.text(), label="Doom resized")
                start = time.perf_counter(); terminal.send(b"q")
                terminal.until(lambda s: "Ctrl+G return" not in s.text() and "DOOM | Q=Pause" not in s.text(), label="Doom pause/close")
                report["doom_close_ms"] = round((time.perf_counter()-start)*1000, 1)
                open_native_doom(terminal)
                terminal.send(b"\x07")
                terminal.until(lambda s: "Ctrl+G return" not in s.text() and "DOOM | Q=Pause" not in s.text(), label="host Ctrl+G rescue")
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


def open_native_doom(terminal):
    """Current command surface: management picker, not the removed /doom alias."""
    terminal.send(b"/extensions")
    terminal.pump(0.15)
    terminal.send(b"\x1b")  # dismiss completion; Enter must submit, not complete
    terminal.pump(0.1)
    terminal.send(b"\r")
    terminal.until(lambda s: "Manage extensions" in s.text(), label="extension management")
    terminal.send(b"octet-pi-compat\r")
    terminal.until(lambda s: "Play DOOM" in s.text(), label="Doom menu item")
    terminal.send(b"\r")
    terminal.until(lambda s: "Arguments for doom" in s.text(), label="Doom arguments")
    start = time.monotonic_ns()
    terminal.send(b"\r")
    terminal.until(lambda s: "Ctrl+G return" in s.text() and "▀" in s.text(), label="Doom open")
    return (time.monotonic_ns()-start)/1e6


def install_frame_trace(args, bundle, output):
    """Test-only instrumentation; no factory, runtime, or global files modified.

    Timestamp before transport serialization; fingerprint three RGB rows. A paired
    uninstrumented run is needed to detect observer overhead. This is NOT paint ACK.
    """
    wrapper = output/"trace-runner.mjs"
    trace = output/"bridge.jsonl"
    source = r'''
import { openSync, writeSync } from 'node:fs';
import { createHash } from 'node:crypto';
const { Transport } = await import(ADAPTER + '/lib/transport.mjs');
const { RemoteTUI } = await import(ADAPTER + '/lib/remote-ui.mjs');
const fd = openSync(TRACE, 'wx', 0o600);
let records = 0, renderNs = 0;
const render = RemoteTUI.prototype.render;
RemoteTUI.prototype.render = function(...args) {
  const start = process.hrtime.bigint();
  try { return render.apply(this, args); }
  finally { renderNs = Number(process.hrtime.bigint() - start); }
};
const send = Transport.prototype.send;
Transport.prototype.send = function(message, ...args) {
  if (message.method === 'ui/frame') {
    const ns = Number(process.hrtime.bigint()), p = message.params;
    const pixels = [];
    for (const line of p.lines.slice(0, 3)) {
      for (const m of line.matchAll(/\x1b\[38;2;(\d+);(\d+);(\d+)m\x1b\[48;2;(\d+);(\d+);(\d+)m▀/g)) {
        pixels.push(...m.slice(1).map(Number));
      }
    }
    if (++records > 10000) throw new Error('bounded performance trace exceeded');
    const hash = pixels.length === p.columns*3*6 ? createHash('sha256').update(Buffer.from(pixels)).digest('hex') : null;
    writeSync(fd, JSON.stringify({ns, surface:p.surface_id, revision:p.revision,
      columns:p.columns, rows:p.rows, hash, render_ns:renderNs,
      observer_ns:Number(process.hrtime.bigint())-ns})+'\n');
  }
  return send.call(this, message, ...args);
};
await import(ADAPTER + '/runner.mjs');
'''.replace("ADAPTER", json.dumps(args.adapter.as_uri())).replace("TRACE", json.dumps(str(trace)))
    wrapper.write_text(source)
    wrapper.chmod(0o600)
    manifest = bundle/"extension.toml"
    manifest.write_text(manifest.read_text().replace(str(args.adapter/"runner.mjs"), str(wrapper)))
    return trace


def correlate_frames(terminal, bridge, resizes):
    """Replay AFTER process exit. No Python VT parsing in throughput windows.

    Exclude repeated source signatures rather than assigning ambiguous lag. Match
    only the first terminal observation of a unique source signature, never a
    later repaint. Repeated scenes/no-op source snapshots are not dropped frames.
    """
    screen = Screen(96, 32, track_colors=True)
    offset = 0
    pending = iter(resizes)
    resize = next(pending, None)
    seen = set()
    records = []
    for frame in terminal.clock.frames:
        while resize and resize["offset"] <= frame["end"]:
            screen.feed(terminal.capture[offset:resize["offset"]])
            offset = resize["offset"]
            screen.resize(resize["columns"], resize["rows"])
            resize = next(pending, None)
        screen.feed(terminal.capture[offset:frame["end"]])
        offset = frame["end"]
        signature = screen.pixel_signature()
        records.append({**frame, "hash": signature,
                        "doom_visible": "▀" in screen.text(),
                        "rescue_visible": "Ctrl+G return" in screen.text(),
                        "first_observation": signature is not None and signature not in seen})
        seen.add(signature)
    by_hash = {}
    for entry in bridge:
        if entry["hash"]:
            by_hash.setdefault(entry["hash"], []).append(entry)
    matches = []
    for frame in records:
        candidates = by_hash.get(frame["hash"], [])
        if frame["first_observation"] and len(candidates) == 1:
            source = candidates[0]
            if frame["ns"] >= source["ns"]:
                matches.append({"source_ns": source["ns"], "terminal_ns": frame["ns"],
                                "revision": source["revision"], "hash": frame["hash"],
                                "lag_ms": (frame["ns"]-source["ns"])/1e6})
    return records, matches


def calibrate_node_clock(node, environment):
    """Bracket hrtime with Python monotonic reads; epochs can differ on macOS."""
    process = subprocess.Popen([str(node), "-e",
        "process.stdin.on('data', () => console.log(process.hrtime.bigint().toString()))"],
        env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    samples = []
    try:
        for _ in range(20):
            before = time.monotonic_ns()
            process.stdin.write(b"\n"); process.stdin.flush()
            if not select.select([process.stdout], [], [], 5)[0]:
                raise AssertionError("clock calibration timed out")
            child = int(process.stdout.readline())
            after = time.monotonic_ns()
            samples.append({"offset_ns": (before+after)//2-child,
                            "uncertainty_ns": (after-before)//2})
        process.stdin.close()
        process.wait(timeout=5)
        return min(samples, key=lambda s: s["uncertainty_ns"])
    finally:
        if process.poll() is None: process.kill(); process.wait(timeout=5)
        process.stdout.close(); process.stderr.close()
        if not process.stdin.closed: process.stdin.close()


def run_doom_performance(args):
    output = args.doom_performance.resolve()
    output.mkdir(parents=True, exist_ok=False)
    output.chmod(0o700)
    report = {"schema": 1, "argv": sys.argv, "host": platform.platform(),
              "machine": platform.machine(), "cpu_count": os.cpu_count(),
              "load_start": os.getloadavg(), "binary": str(args.binary),
              "binary_sha256": sha256(args.binary), "harness_sha256": sha256(__file__),
              "node": str(args.node), "node_sha256": sha256(args.node),
              "adapter": str(args.adapter), "entrypoint": str(args.doom),
              "entrypoint_sha256": sha256(args.doom), "trace_bridge": args.trace_bridge,
              "geometry": [96, 32], "samples": [], "actions": [], "resizes": [],
              "functional": "not completed", "smooth_35fps_gate": "unqualified",
              "displayed_fps": None, "n4020_evidence": False,
              "limits": ["PTY read timestamps are emitted output, not displayed frames or paint ACKs.",
                         "Current host only; no N4020 qualification or Tern/native display interaction.",
                         "Lag starts before adapter transport serialization, after component rendering.",
                         "Unique three-row RGB matches only; repeated source frames excluded.",
                         "Control observation timings include VT parsing and predicate polling."]}
    report["adapter_hashes"] = {str(p.relative_to(args.adapter)): sha256(p)
                                for p in sorted(args.adapter.rglob("*.mjs"))
                                if "node_modules" not in p.parts and ".pi-acceptance" not in p.parts}
    package = args.doom.parent.parent
    report["doom_assets"] = {str(p): sha256(p) for p in
                             [*sorted(args.doom.parent.glob("*.ts")), package/"package.json",
                              package/"doom1.wad", *sorted((package/"doom/build").glob("doom.*"))]
                             if p.is_file()}
    terminal = None
    trace = None
    with tempfile.TemporaryDirectory(prefix="octet-doom-performance-") as name:
        root = Path(name).resolve()
        environment, workspace = isolated(root)
        environment["OCTET_TERN"] = "off"
        try:
            extension_root = configure(args.adapter, args.node, root, [args.doom], environment, workspace)
            if args.trace_bridge:
                report["clock_calibration_start"] = calibrate_node_clock(args.node, environment)
                trace = install_frame_trace(args, extension_root/"octet-pi-compat", output)
            command = native_command(args.binary, extension_root)
            report["command"] = command
            terminal = TerminalProcess(command, environment, workspace, capture_limit=PERFORMANCE_LIMIT)
            terminal.until(lambda s: "probe" in s.text(), label="native ready")
            report["open_observed_ms"] = open_native_doom(terminal)
            rgb = terminal.screen.rgb
            terminal.until(lambda s: s.rgb > rgb+500, label="populated Doom framebuffer")
            terminal.send(b"\x1b"); terminal.pump(0.4)
            terminal.send(b"\x1b[27;1:3u"); terminal.pump(0.1)
            for _ in range(3):
                terminal.send(b"\r\x1b[13;1:3u"); terminal.pump(0.4)
            terminal.pump(1)

            def window(label, seconds):
                terminal.defer_screen = True
                start = time.monotonic_ns()
                terminal.pump(seconds)
                end = time.monotonic_ns()
                result = {"label": label, **frame_window(terminal.clock, start, end)}
                report["samples"].append(result)
                return result  # defer VT parsing until the turn key has been released

            def key(data, label):
                event = {"label": label, "ns": time.monotonic_ns(), "key_hex": data.hex()}
                report["actions"].append(event)
                terminal.send(data)
                return event

            def wait_after(event, predicate):
                terminal.until(predicate, timeout=10, label=event["label"])
                event["observed_ns"] = time.monotonic_ns()
                event["observed_ms"] = (event["observed_ns"]-event["ns"])/1e6
                event["screen"] = terminal.screen.text()

            for i in range(args.samples):
                key(b"\x1b[C", f"turn-{i+1}")
                terminal.pump(0.3)
                turning = window(f"turn-{i+1}", args.seconds)
                key(b"\x1b[1;1:3C", f"release-{i+1}")
                terminal.pump(0.3)
                released = window(f"released-{i+1}", 0.7)
                terminal.defer_screen = False
                terminal.observe()
                if turning["output_fps"] < 5:
                    raise AssertionError("Doom did not repaint while turning")
                if released["output_fps"] >= turning["output_fps"]/2:
                    raise AssertionError("Doom camera did not stop after release")
            pause = key(b"q", "pause-to-composer")
            wait_after(pause, lambda s: "Ctrl+G return" not in s.text() and "DOOM | Q=Pause" not in s.text())
            report["resume_observed_ms"] = open_native_doom(terminal)
            key(b"\x1b[C", "resumed-turn")
            terminal.pump(0.3)
            window("resumed-turn", args.seconds)
            key(b"\x1b[1;1:3C", "before-resize-release")
            terminal.pump(0.3)
            terminal.defer_screen = False
            terminal.observe()
            report["resizes"].append({"offset": len(terminal.capture), "ns": time.monotonic_ns(), "columns": 80, "rows": 28})
            frames = terminal.screen.frames
            terminal.resize(80, 28)
            terminal.until(lambda s: s.frames > frames and "▀" in s.text() and "Ctrl+G return" in s.text(), label="resized active mount")
            key(b"\x1b[C", "resized-turn")
            terminal.pump(0.3)
            window("resized-turn", args.seconds)
            key(b"\x1b[1;1:3C", "resized-release")
            terminal.pump(0.2)
            terminal.defer_screen = False
            terminal.observe()
            rescue = key(b"\x07", "host-rescue")
            wait_after(rescue, lambda s: "Ctrl+G return" not in s.text() and "DOOM | Q=Pause" not in s.text())
            terminal.send(b"post-rescue-edit")
            terminal.until(lambda s: "post-rescue-edit" in s.text(), label="post-rescue editable composer")
            terminal.send(b"\x03"); terminal.pump(0.1)
            terminal.close()
            report["functional"] = "pass: input/release/pause/resume/resize/rescue/editor/shutdown/termios"
        except (AssertionError, OSError, subprocess.SubprocessError) as error:
            report["error"] = str(error)
            if isinstance(error, subprocess.CalledProcessError) and error.stderr:
                report["stderr"] = error.stderr.decode(errors="replace")
        finally:
            if terminal:
                terminal.dispose()
                capture = output/"terminal.pty"
                capture.write_bytes(terminal.capture); capture.chmod(0o600)
                report["capture_sha256"] = sha256(capture)
                report["captured_bytes"] = len(terminal.capture)
                report["first_frame_ms"] = terminal.first_frame_ms
                (output/"clock.json").write_text(json.dumps({"reads": terminal.clock.reads, "frames": terminal.clock.frames}))
                bridge = [json.loads(line) for line in trace.read_text().splitlines()] if trace and trace.exists() else []
                if bridge:
                    report["clock_calibration_end"] = calibrate_node_clock(args.node, environment)
                    calibration = report["clock_calibration_start"]
                    report["clock_offset_drift_ns"] = report["clock_calibration_end"]["offset_ns"]-calibration["offset_ns"]
                    for entry in bridge: entry["ns"] += calibration["offset_ns"]
                observed, matches = correlate_frames(terminal, bridge, report["resizes"])
                (output/"observed.json").write_text(json.dumps(observed))
                for action in report["actions"]:
                    later = [f for f in observed if f["ns"] >= action["ns"]]
                    if action["label"] in ("pause-to-composer", "host-rescue"):
                        response = next((f for f in later if not f["rescue_visible"] and not f["doom_visible"]), None)
                        if response: action["input_to_closed_frame_ms"] = (response["ns"]-action["ns"])/1e6
                    elif action["label"].startswith("turn-") or action["label"] in ("resumed-turn", "resized-turn"):
                        previous = next((f["hash"] for f in reversed(observed) if f["ns"] < action["ns"]), None)
                        response = next((f for f in later if f["hash"] and f["hash"] != previous), None)
                        if response: action["input_to_next_changed_rgb_ms"] = (response["ns"]-action["ns"])/1e6
                if bridge:
                    report["bridge_trace_sha256"] = sha256(trace)
                    report["matched_lag_ms"] = distribution([m["lag_ms"] for m in matches])
                    (output/"matches.json").write_text(json.dumps(matches))
                for sample in report["samples"]:
                    start, end = sample["start_ns"], sample["end_ns"]
                    emitted = [b for b in bridge if start <= b["ns"] < end]
                    shown = [f for f in observed if start <= f["ns"] < end]
                    # Distinct sampled RGB snapshots, NOT physical display frames.
                    sample["distinct_rgb_signatures"] = len({f["hash"] for f in shown if f["hash"]})
                    if bridge:
                        sample["extension_snapshots"] = len(emitted)
                        sample["extension_snapshot_hz"] = len(emitted)/sample["seconds"]
                        sample["render_ms"] = distribution([b["render_ns"]/1e6 for b in emitted])
                        sample["trace_observer_ms"] = distribution([b["observer_ns"]/1e6 for b in emitted])
                        sample["matched_lag_ms"] = distribution([m["lag_ms"] for m in matches if start <= m["source_ns"] <= m["terminal_ns"] < end])
                report["load_end"] = os.getloadavg()
            (output/"report.json").write_text(json.dumps(report, indent=2)+"\n")
    print(json.dumps({"report": str(output/"report.json"), "functional": report["functional"],
                      "error": report.get("error"), "smooth_35fps_gate": "unqualified"}, indent=2))
    return "error" not in report


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
    parser.add_argument("--doom-performance", type=Path, help="new private evidence directory; actual Octet Doom throughput/lifecycle probe")
    parser.add_argument("--seconds", type=float, default=2, help="seconds per moving window (1..5)")
    parser.add_argument("--trace-bridge", action="store_true", help="test-only adapter timestamps/profiling; pair with uninstrumented run")
    parser.add_argument("--expect-binary-sha256", help="refuse execution if the binary hash differs")
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
    if args.expect_binary_sha256 and sha256(args.binary) != args.expect_binary_sha256:
        parser.error("binary SHA-256 does not match --expect-binary-sha256")
    if args.trace_bridge and not args.doom_performance:
        parser.error("--trace-bridge requires --doom-performance")
    if args.doom_performance:
        if not args.doom or len(entries) != 1 or args.pi_cli:
            parser.error("--doom-performance requires --doom alone (not other entrypoints or --pi-cli)")
        if not 1 <= args.seconds <= 5 or args.samples > 5:
            parser.error("performance requires --seconds 1..5 and --samples 1..5")
        if not run_doom_performance(args): sys.exit(1)
        return
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

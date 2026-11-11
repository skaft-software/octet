#!/usr/bin/env python3
"""Offline Linux long-session benchmark of a real octet process (stdlib only)."""
from __future__ import annotations

import argparse
import fcntl
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import math
import os
from pathlib import Path
import platform
import pty
import re
import select
import shutil
import statistics
import struct
import subprocess
import tempfile
import termios
import threading
import time

from session_bench import FRAME_END, MODEL, PeakSampler, command, content_text, proc_sample, setup

SCHEMA = "octet.long-session.v1"
SCALES = (100, 1000, 5000)
LARGE_EVERY = 25
# Fixed completion drain after each painted answer; excluded from `settled_ms`.
DRAIN_MS = 40
# Many short lines exercise tool capture without triggering per-line clipping.
LARGE_DATA = ("octet tool fixture " + "x" * 108 + "\n") * 1024
assert len(LARGE_DATA.encode()) == 128 * 1024
PROMPT = re.compile(r"^LONG_PROMPT_(\d{5})$")
PERF = re.compile(rb"octet-perf: ([a-z_.]+) elapsed=(\d+)us bytes=(\d+)")
PHASE = re.compile(rb"octet-startup: ([\w.]+) elapsed=(\d+)us")


def answer(index, streaming=False):
    body = ("A **verified** result with `code`.\n\n" * (512 if streaming else 4))
    return body + f"LONG_REPLY_{index:05}"


def octet_attributed_ms(wall_ms, fixture_ms):
    """Latency attributable to octet: the fake provider's decode/validation time
    grows with the history it checks and must not be charged to the binary."""
    return wall_ms - fixture_ms


def percentile(values, fraction):
    ordered = sorted(values)
    position = (len(ordered) - 1) * fraction
    low, high = math.floor(position), math.ceil(position)
    return ordered[low] + (ordered[high] - ordered[low]) * (position - low)


def distribution(values):
    if not values:
        raise ValueError("empty metric distribution")
    return {"count": len(values), "p50": percentile(values, .5),
            "p95": percentile(values, .95), "max": max(values)}


def growth_per_100(samples):
    xs = [row["turn"] for row in samples]
    ys = [row["rss_bytes"] for row in samples]
    mx, my = statistics.mean(xs), statistics.mean(ys)
    denominator = sum((x - mx) ** 2 for x in xs)
    if not denominator:
        raise ValueError("RSS slope needs at least two turns")
    return 100 * sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / denominator


class Fixture:
    """Check every provider-visible message, not only the most recent marker.

    The validated message list is retained and compared structurally, so a
    dropped, reordered, or rewritten turn fails on the request that carries it.
    Comparing parsed messages instead of re-serialising every one keeps fixture
    cost from growing with the history it measures; that cost is reported
    separately because Python checking is not octet CPU.
    """
    def __init__(self, scales):
        self.scales = set(scales)
        self.messages = []
        self.next_turn = 1
        self.waiting_tool = False
        self.rows = []
        self.errors = []
        self.lock = threading.Lock()

    def plan(self, body, received_ns, wire_bytes, decode_ms):
        if body.get("model") != MODEL or body.get("stream") is not True:
            raise AssertionError("unexpected model or nonstreaming request")
        messages = body["messages"]
        if messages[:len(self.messages)] != self.messages:
            raise AssertionError("provider-visible history changed or was dropped")
        suffix = messages[len(self.messages):]
        # The first request has the fixed system instruction before the first user.
        if not self.messages and suffix and suffix[0]["role"] == "system":
            suffix = suffix[1:]
        index = self.next_turn
        tool = index % LARGE_EVERY == 0
        if self.waiting_tool:
            if len(suffix) != 2 or suffix[0]["role"] != "assistant" or suffix[1]["role"] != "tool":
                raise AssertionError("missing or extra tool call/result")
            calls = suffix[0].get("tool_calls", [])
            if len(calls) != 1 or calls[0]["function"]["name"] != "bash":
                raise AssertionError("tool call changed")
            if json.loads(calls[0]["function"]["arguments"]) != {"command": "cat bench-large.txt"}:
                raise AssertionError("tool arguments changed")
            if suffix[1].get("tool_call_id") != calls[0]["id"]:
                raise AssertionError("tool pairing changed")
            output = content_text(suffix[1].get("content"))
            if "exit=0" not in output or LARGE_DATA not in output:
                raise AssertionError("large tool output missing, truncated, or unsuccessful")
        else:
            if self.messages:
                if len(suffix) != 2 or suffix[0].get("role") != "assistant":
                    raise AssertionError("previous answer or new prompt missing")
                if content_text(suffix[0].get("content")) != answer(index - 1, index - 1 in self.scales):
                    raise AssertionError("completed answer changed before provider replay")
                suffix = suffix[1:]
            if len(suffix) != 1 or suffix[0].get("role") != "user":
                raise AssertionError("unexpected request (compaction/retry/discovery is not a turn)")
            if content_text(suffix[0].get("content")) != f"LONG_PROMPT_{index:05}":
                raise AssertionError("turn missing, duplicated, or reordered")
        self.messages = messages
        calls = tool and not self.waiting_tool
        self.rows.append({"turn": index, "tool_call": calls, "received_ns": received_ns,
                          "wire_bytes": wire_bytes, "decode_ms": decode_ms,
                          "validated_ns": time.monotonic_ns()})
        if calls:
            self.waiting_tool = True
        else:
            self.waiting_tool = False
            self.next_turn += 1
        return index, calls


class Provider(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, scales):
        self.fixture = Fixture(scales)
        super().__init__(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.shutdown()
        self.server_close()
        self.thread.join()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def do_POST(self):
        try:
            if self.path != "/v1/chat/completions":
                raise AssertionError("unexpected provider route")
            length = int(self.headers.get("Content-Length", 0))
            if not 0 < length <= 64 * 1024 * 1024:
                raise AssertionError("provider request outside fixture bound")
            raw = self.rfile.read(length)
            received = time.monotonic_ns()
            body = json.loads(raw)
            decoded = time.monotonic_ns()
            fixture = self.server.fixture
            with fixture.lock:
                index, calls = fixture.plan(body, received, length, (decoded - received) / 1e6)
                row = fixture.rows[-1]
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Connection", "close")
            self.end_headers()

            def chunk(delta, finish=None):
                payload = {"id": f"long-{index}", "object": "chat.completion.chunk", "model": MODEL,
                           "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
                self.wfile.write(b"data: " + json.dumps(payload).encode() + b"\n\n")
                self.wfile.flush()

            chunk({"role": "assistant", "content": ""})
            row["stream_start_ns"] = time.monotonic_ns()
            if calls:
                chunk({"tool_calls": [{"index": 0, "id": f"call-{index}", "type": "function",
                       "function": {"name": "bash", "arguments": json.dumps({"command": "cat bench-large.txt"})}}]})
            else:
                text = answer(index, index in fixture.scales)
                for position in range(0, len(text), 512):
                    # Fixed elapsed-time schedule, never consumer-ack paced.
                    if index in fixture.scales:
                        target = row["stream_start_ns"] / 1e9 + (position // 512) * .008
                        time.sleep(max(0, target - time.monotonic()))
                    chunk({"content": text[position:position + 512]})
            chunk({}, "tool_calls" if calls else "stop")
            usage = {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}
            self.wfile.write(b"data: " + json.dumps({"id": f"long-{index}", "model": MODEL,
                              "choices": [], "usage": usage}).encode() + b"\n\ndata: [DONE]\n\n")
            self.wfile.flush()
            row["stream_end_ns"] = time.monotonic_ns()
        except Exception as error:
            with self.server.fixture.lock:
                self.server.fixture.errors.append(str(error))
            self.send_error(500, "fixture validation failed")
        finally:
            self.close_connection = True


def traces(raw):
    metrics = {}
    for phase, elapsed, size in PERF.findall(raw):
        metrics.setdefault(phase.decode(), []).append({"wall_ms": int(elapsed) / 1000, "bytes": int(size)})
    return metrics


class Ledger:
    """Incremental tail reader for the durable session file.

    A turn is complete when its checkpoint record is durable, not when its text
    is painted: the renderer can show a finished answer while the run is still
    settling. Submitting the next prompt before that boundary overlaps two runs
    and loses checkpoints, so every turn waits for its own record here.
    """
    def __init__(self, path):
        self.stream = path.open("rb")
        self.pending = b""
        self.checkpoints = 0

    def advance(self):
        data = self.stream.read()
        if data:
            self.pending += data
            *lines, self.pending = self.pending.split(b"\n")
            for line in lines:
                record = json.loads(line)
                if record["type"] == "checkpoint":
                    self.checkpoints += 1
        return self.checkpoints

    def wait_checkpoints(self, expected, timeout):
        deadline = time.monotonic() + timeout
        while self.advance() < expected:
            if time.monotonic() > deadline:
                raise TimeoutError(
                    f"turn did not settle: {self.checkpoints} durable checkpoints, expected {expected}")
            time.sleep(.001)

    def close(self):
        self.stream.close()


class Terminal:
    def __init__(self, binary, workspace, env, session, errors, timeout, strace=None):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        self.original = termios.tcgetattr(self.master)
        self.timeout = timeout
        self.tail = b""
        self.total_bytes = 0
        self.frames = 0
        self.errors = errors
        self.error_offset = 0
        args = command(binary) + ["--workspace", str(workspace), "--session", str(session), "--cache-warming", "off",
                                 "--max-output-bytes", "200000", "--system-prompt", "Deterministic offline fixture."]
        if strace:
            args = ["strace", "-f", "-qq", "-T", "-y", "-e", "trace=write,fdatasync", "-o", str(strace), *args]

        def controlling_tty():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        self.started = time.monotonic_ns()
        try:
            self.child = subprocess.Popen(args, cwd=workspace, env=env, stdin=slave, stdout=slave,
                                          stderr=errors, preexec_fn=controlling_tty)
        finally:
            os.close(slave)
        self.pid = self.child.pid
        if strace:
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                children = Path(f"/proc/{self.pid}/task/{self.pid}/children").read_text().split()
                if children:
                    self.pid = int(children[0])
                    break
                time.sleep(.002)
            else:
                self.close()
                raise TimeoutError("strace child did not start")
        self.sampler = PeakSampler(self.pid)

    def read(self, wait=.01):
        if self.child.poll() is not None:
            raise RuntimeError(f"octet exited {self.child.returncode}; {self.error_tail()!r}")
        if not select.select([self.master], [], [], wait)[0]:
            return b""
        data = os.read(self.master, 65536)
        scan = self.tail + data
        if b"\x1b[6n" in scan and b"\x1b[6n" not in self.tail:
            os.write(self.master, b"\x1b[1;1R")
        self.frames += scan.count(FRAME_END) - self.tail.count(FRAME_END)
        self.total_bytes += len(data)
        self.tail = scan[-256:]
        return data

    def frame(self, marker):
        markers = [marker] if isinstance(marker, bytes) else marker
        seen = set()
        scan = b""
        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
            scan = scan[-256:] + self.read()
            for item in markers:
                position = scan.find(item)
                if position >= 0:
                    seen.add(item)
                    scan = scan[position:]
            if len(seen) == len(markers) and FRAME_END in scan:
                return (time.monotonic_ns() - self.started) / 1e6
        raise TimeoutError(f"no frame for {marker!r}; tail={self.tail!r}; stderr={self.error_tail()!r}")

    def quiet(self):
        # A bounded completion drain, not an unbounded animation-dependent wait.
        end = time.monotonic() + DRAIN_MS / 1000
        while time.monotonic() < end:
            self.read(.002)

    def error_tail(self):
        return os.pread(self.errors.fileno(), 3000, max(0, os.fstat(self.errors.fileno()).st_size - 3000))

    def take_traces(self):
        size = os.fstat(self.errors.fileno()).st_size
        raw = os.pread(self.errors.fileno(), size - self.error_offset, self.error_offset)
        self.error_offset = size
        return traces(raw)

    def close(self):
        try:
            if self.child.poll() is None:
                os.write(self.master, b"\x04")
                deadline = time.monotonic() + self.timeout
                while self.child.poll() is None and time.monotonic() < deadline:
                    if select.select([self.master], [], [], .01)[0]:
                        try:
                            os.read(self.master, 65536)
                        except OSError:
                            pass
                if self.child.poll() is None:
                    raise TimeoutError("octet shutdown timed out")
            if self.child.returncode != 0:
                raise AssertionError(f"octet shutdown exit {self.child.returncode}")
            if termios.tcgetattr(self.master) != self.original:
                raise AssertionError("octet did not restore terminal modes")
        finally:
            if hasattr(self, "sampler"):
                self.sampler.close()
            if self.child.poll() is None:
                # Includes a wrapped profiler and its tracee; all are run-owned.
                import signal
                os.killpg(self.child.pid, signal.SIGKILL)
                self.child.wait()
            os.close(self.master)


def check_session(path, turns, scales):
    """Independently verify durable output, tool bytes, and parent/head links."""
    ids, parent = set(), None
    users, replies, tools = [], [], 0
    checkpoints = 0
    with path.open() as stream:
        for line in stream:
            record = json.loads(line)
            kind = record["type"]
            if kind == "entry":
                if record["id"] in ids or record["parent"] != parent:
                    raise AssertionError("session entry lost, duplicated, or reparented")
                ids.add(record["id"])
                parent = record["id"]
                value = record["value"]
                if value["type"] != "message":
                    continue
                if "Assistant" in value:
                    text = "".join(part.get("Text", "") for part in value["Assistant"]["content"])
                    if text:
                        replies.append(text)
                if "User" in value:
                    for part in value["User"]["content"]:
                        if "Text" in part:
                            users.append(part["Text"])
                        if "ToolResult" in part:
                            result = part["ToolResult"]
                            text = "".join(p.get("Text", "") for p in result["content"])
                            if result["is_error"] or LARGE_DATA not in text or "exit=0" not in text:
                                raise AssertionError("durable tool output changed")
                            tools += 1
            elif kind == "head" and record["id"] != parent:
                raise AssertionError("session head does not select the durable tail")
            elif kind == "checkpoint":
                checkpoints += 1
    if users != [f"LONG_PROMPT_{i:05}" for i in range(1, turns + 1)]:
        raise AssertionError("durable user prompts differ")
    if replies != [answer(i, i in scales) for i in range(1, turns + 1)]:
        raise AssertionError("durable assistant output differs")
    if tools != turns // LARGE_EVERY or checkpoints != turns:
        raise AssertionError("missing tools or completed-turn checkpoints")
    return {"users": turns, "answers": turns, "large_tools": tools, "checkpoints": checkpoints}


def resume(binary, paths, session, turns, timeout):
    _, workspace, env = paths
    env = dict(env, OCTET_STARTUP_TRACE="1")
    with tempfile.TemporaryFile() as errors:
        tty = Terminal(binary, workspace, env, session, errors, timeout)
        try:
            # Fullscreen restores a bounded visible tail, not native scrollback.
            first = tty.frame([f"LONG_REPLY_{turns:05}".encode(), MODEL.encode()])
            if termios.tcgetattr(tty.master)[3] & (termios.ICANON | termios.ECHO):
                raise AssertionError("resume composer is not in raw mode")
            os.write(tty.master, b"EDITABLE_MARKER")
            tty.frame(b"EDITABLE_MARKER")
            raw = os.pread(errors.fileno(), os.fstat(errors.fileno()).st_size, 0)
            return {"ready_frame_ms": first, "rss_bytes": proc_sample(tty.pid)["rss_bytes"],
                    "phase_ms": {k.decode(): int(v) / 1000 for k, v in PHASE.findall(raw)}}
        finally:
            tty.close()


def summarize(samples):
    ordinary = [row for row in samples if row["kind"] == "small"]
    window = min(25, len(ordinary) // 2)
    by_kind = {kind: distribution([row["wall_ms"] for row in samples if row["kind"] == kind])
               for kind in {row["kind"] for row in samples}}
    append = [row["append_ms"] for row in samples if row["append_ms"] is not None]
    return {"turn_latency_ms": distribution([row["wall_ms"] for row in samples]),
            "reply_frame_ms": distribution([row["reply_frame_ms"] for row in samples]),
            "settled_ms": distribution([row["settled_ms"] for row in samples]),
            "latency_by_kind_ms": by_kind,
            "early_small_p50_ms": statistics.median(row["wall_ms"] for row in ordinary[:window]),
            "late_small_p50_ms": statistics.median(row["wall_ms"] for row in ordinary[-window:]),
            "early_small_reply_p50_ms": statistics.median(row["reply_frame_ms"] for row in ordinary[:window]),
            "late_small_reply_p50_ms": statistics.median(row["reply_frame_ms"] for row in ordinary[-window:]),
            "early_small_settled_p50_ms": statistics.median(row["settled_ms"] for row in ordinary[:window]),
            "late_small_settled_p50_ms": statistics.median(row["settled_ms"] for row in ordinary[-window:]),
            "peak_rss_bytes": max(row["peak_rss_bytes"] for row in samples),
            "end_rss_bytes": samples[-1]["rss_bytes"],
            "rss_growth_bytes_per_100_turns": growth_per_100(samples),
            "session_bytes": samples[-1]["session_bytes"],
            "append_bytes_per_turn": distribution([row["append_bytes"] for row in samples]),
            "append_persist_ms_per_turn": distribution(append) if append else None,
            "cpu_ms": sum(row["cpu_ms"] for row in samples),
            "stream": [row for row in samples if row["kind"] == "stream"][-1]}


def run(binary, scales, work, timeout, strace=False):
    provider = Provider(scales)
    home, workspace, env = setup(work, provider.server_port)
    # An intentionally un-compacted history exposes whole-history costs. The
    # synthetic large window is fixture metadata, not a production model claim.
    config = home / ".octet/credentials/custom.json"
    registry = json.loads(config.read_text())
    registry["providers"]["bench"]["models"][0]["context_window"] = 100_000_000
    config.write_text(json.dumps(registry))
    env.update(OCTET_COMPACTION_MODE="disabled", OCTET_PERF_TRACE="1")
    (workspace / "bench-large.txt").write_text(LARGE_DATA)
    session = work / "live.jsonl"
    samples, cells = [], []
    try:
        ledger = None
        with (work / "stderr.log").open("w+b") as errors:
            tty = Terminal(binary, workspace, env, session, errors, timeout,
                           work / "syscalls.log" if strace else None)
            try:
                ready = tty.frame(MODEL.encode())
                tty.quiet()
                tty.take_traces()
                previous_size = session.stat().st_size
                ledger = Ledger(session)
                for index in range(1, max(scales) + 1):
                    before = proc_sample(tty.pid)
                    frame_count, output_bytes = tty.frames, tty.total_bytes
                    started = time.monotonic_ns()
                    os.write(tty.master, f"LONG_PROMPT_{index:05}\r".encode())
                    tty.frame(f"LONG_REPLY_{index:05}".encode())
                    replied = time.monotonic_ns()
                    tty.quiet()
                    # The answer is painted before the run settles; a durable
                    # checkpoint is the boundary that makes turns sequential.
                    ledger.wait_checkpoints(index, timeout)
                    finished = time.monotonic_ns()
                    after = proc_sample(tty.pid)
                    metrics = tty.take_traces()
                    appends = metrics.get("session.persist", [])
                    frames = metrics.get("render.frame", [])
                    size = session.stat().st_size
                    with provider.fixture.lock:
                        if provider.fixture.errors:
                            raise AssertionError(provider.fixture.errors)
                        requests = [row.copy() for row in provider.fixture.rows if row["turn"] == index]
                    validation_ms = sum((row["validated_ns"] - row["received_ns"]) / 1e6 for row in requests)
                    row = {"turn": index, "kind": "stream" if index in scales else
                           ("large_tool" if index % LARGE_EVERY == 0 else "small"),
                           # The fake provider's JSON decode and history check grow with the
                           # history it verifies; that is the fixture's cost, not octet's, so
                           # it is subtracted from every gated latency and kept visible raw.
                           "wall_ms": octet_attributed_ms((finished - started) / 1e6, validation_ms),
                           "reply_frame_ms": octet_attributed_ms((replied - started) / 1e6, validation_ms),
                           "settled_ms": octet_attributed_ms((finished - started) / 1e6 - DRAIN_MS, validation_ms),
                           "wall_ms_raw": (finished - started) / 1e6,
                           "provider_validation_ms": validation_ms,
                           "cpu_ms": after["cpu_ms"] - before["cpu_ms"],
                           "rss_bytes": after["rss_bytes"], "private_bytes": after["private_bytes"],
                           "peak_rss_bytes": tty.sampler.peak_rss,
                           "session_bytes": size, "append_bytes": size - previous_size,
                           "append_ms": sum(item["wall_ms"] for item in appends) if appends else None,
                           "render_wall_ms": distribution([item["wall_ms"] for item in frames]) if frames else None,
                           "render_total_ms": sum(item["wall_ms"] for item in frames) if frames else None,
                           "pty_frames": tty.frames - frame_count, "pty_bytes": tty.total_bytes - output_bytes,
                           "request_bytes": sum(item["wire_bytes"] for item in requests),
                           "submit_to_request_ms": (requests[0]["received_ns"] - started) / 1e6,
                           "trace": metrics}
                    samples.append(row)
                    previous_size = size
                    if index % 100 == 0:
                        print(f"octet: {index}/{max(scales)} turns", flush=True)
                    if index in scales:
                        checks = check_session(session, index, scales)
                        snapshot = work / f"resume-{index}.jsonl"
                        shutil.copyfile(session, snapshot)
                        snapshot.chmod(0o600)
                        resumed = resume(binary, (home, workspace, env), snapshot, index, timeout)
                        cells.append({"turns": index, "summary": summarize(samples),
                                      "resume": resumed, "correctness": checks})
            finally:
                if ledger is not None:
                    ledger.close()
                tty.close()
        expected = max(scales) + max(scales) // LARGE_EVERY
        if provider.fixture.next_turn != max(scales) + 1 or len(provider.fixture.rows) != expected:
            raise AssertionError("provider exchange count differs")
        return {"startup_ready_ms": ready, "cells": cells, "samples": samples,
                "provider_requests": provider.fixture.rows, "correctness_passed": True}
    except Exception as error:
        if provider.fixture.errors:
            raise AssertionError(f"{error}; provider: {provider.fixture.errors}") from error
        raise
    finally:
        provider.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--scales", type=int, nargs="+", default=list(SCALES))
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--work-dir", type=Path, help="new directory; retain sessions and diagnostics")
    parser.add_argument("--strace", action="store_true", help="diagnostic run only; adds syscall observer overhead")
    args = parser.parse_args()
    if sorted(set(args.scales)) != args.scales or args.scales[0] < 10 or args.scales[-1] > 5000:
        parser.error("scales must be increasing unique integers in 10..5000")
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("timeout must be positive and finite")
    binary = args.binary.resolve(strict=True)
    identity = hashlib.sha256(binary.read_bytes()).hexdigest()
    result = {"schema": SCHEMA, "status": "failed", "binary_sha256": identity,
              "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "system": platform.platform(), "machine": platform.machine(),
              "python": platform.python_version(), "scales": args.scales, "viewport": [120, 40],
              "clock_ticks_per_second": os.sysconf("SC_CLK_TCK"), "strace": args.strace,
              "fixture": {"large_every": LARGE_EVERY, "large_bytes": len(LARGE_DATA.encode()),
                          "large_sha256": hashlib.sha256(LARGE_DATA.encode()).hexdigest(),
                          "compaction": "disabled", "extensions": [], "stream_chunk_bytes": 512,
                          "stream_chunk_interval_ms": 8},
              "scope": {"latency": "submit to answer PTY frame plus fixed 40ms completion drain, minus the fake provider's own decode/validation time (raw value kept as wall_ms_raw); includes the scheduled stream delay",
                        "reply_frame": "submit to the first answer PTY frame, excluding the fixed completion drain",
                        "settled": "submit to the durable checkpoint record that ends the run, excluding the fixed drain",
                        "resume": "spawn to history/model-ready synchronized fullscreen PTY frame, not first provisional composer or emulator paint",
                        "cpu": "own octet process /proc ticks; excludes Python provider and tool descendants",
                        "rss": "own octet process; peak sampled every 2ms, may miss brief peaks; end=settled final turn, before exit",
                        "append": "traced SessionWriter::persist (lock/write/sync), excluding entry encoding; null on uninstrumented binaries",
                        "render": "traced renderer request_render_force plus frame_written_at wall time, including writes/lock waits; null on uninstrumented binaries"}}
    try:
        if args.work_dir:
            args.work_dir.mkdir(parents=True, mode=0o700)
            result.update(run(binary, args.scales, args.work_dir.resolve(), args.timeout, args.strace))
        else:
            with tempfile.TemporaryDirectory(prefix="octet-long-session-") as directory:
                result.update(run(binary, args.scales, Path(directory), args.timeout, args.strace))
        if hashlib.sha256(binary.read_bytes()).hexdigest() != identity:
            raise AssertionError("binary changed during benchmark")
        result["status"] = "passed"
    except Exception as error:
        result["failure"] = f"{type(error).__name__}: {error}"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n")
    print(json.dumps({"status": result["status"], "output": str(args.output)}), flush=True)
    return 0 if result["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())

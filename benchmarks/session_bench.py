#!/usr/bin/env python3
"""Linux PTY and loopback-provider benchmarks for an already-built octet binary."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import pty
import re
import select
import signal
import statistics
import struct
import subprocess
import tempfile
import termios
import threading
import time
import fcntl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MODEL = "bench-model"
FRAME_END = b"\x1b[?2026l"
PROMPT_RE = re.compile(r"BENCH_PROMPT_(\d{3})")
FILE_DATA = "w" * 200_000
STREAM_DATA = "s" * 60_000


def content_text(value):
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return " ".join(item.get("text", "") for item in value if isinstance(item, dict))
    return ""


def response_plan(scenario, body):
    """Plan from the current user message and its subsequent tool results."""
    messages = body.get("messages", [])
    users = [i for i, item in enumerate(messages) if item.get("role") == "user"]
    user_index = users[-1] if users else -1
    prompt = content_text(messages[user_index].get("content")) if users else ""
    match = PROMPT_RE.search(prompt)
    index = int(match.group(1)) if match else 0
    tools = {item.get("function", {}).get("name") for item in body.get("tools", [])}
    results = sum(item.get("role") == "tool" for item in messages[user_index + 1:])
    calls = []
    if scenario == "tools" and results < 4 and "bash" in tools:
        calls = [("bash", {"command": f"printf step-{results}"})]
    elif scenario == "write" and results == 0 and "write" in tools:
        calls = [("write", {"path": "bench-output.txt", "content": FILE_DATA})]
    if scenario == "stream":
        text = STREAM_DATA
    elif scenario in ("interactive", "long"):
        text = ("A short result with **formatted text** and `code`. " * 40) + f" BENCH_REPLY_{index:03}"
    else:
        text = "BENCH_DONE"
    return calls, text, results, index


class Provider(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.scenario = "answer"
        self.exchanges = []
        self.lock = threading.Lock()
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    def reset(self, scenario):
        with self.lock:
            self.scenario = scenario
            self.exchanges = []

    def snapshot(self):
        with self.lock:
            return list(self.exchanges)

    def close(self):
        self.shutdown()
        self.server_close()
        self.thread.join()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def do_POST(self):
        if self.path.rstrip("/") != "/v1/chat/completions":
            self.send_error(404)
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 32 * 1024 * 1024:
                raise ValueError("invalid request length")
            body = json.loads(self.rfile.read(length))
            scenario = self.server.scenario
            calls, text, results, index = response_plan(scenario, body)
            messages = body.get("messages", [])
            user_index = max((i for i, item in enumerate(messages) if item.get("role") == "user"), default=-1)
            tool_outputs = [content_text(item.get("content")) for item in messages[user_index + 1:]
                            if item.get("role") == "tool"] if scenario == "tools" else []
            with self.server.lock:
                self.server.exchanges.append({"prompt": index, "tool_results": results,
                                              "tool_outputs": tool_outputs, "tool_calls": len(calls),
                                              "stream": bool(body.get("stream"))})
            call_objects = [{"index": i, "id": f"call_{index}_{results}_{i}", "type": "function",
                             "function": {"name": name, "arguments": json.dumps(args)}}
                            for i, (name, args) in enumerate(calls)]
            finish = "tool_calls" if calls else "stop"
            usage = {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}
            if not body.get("stream"):
                message = {"role": "assistant", "content": None if calls else text}
                if calls:
                    message["tool_calls"] = [{k: v for k, v in item.items() if k != "index"} for item in call_objects]
                result = {"id": "bench", "object": "chat.completion", "model": MODEL,
                          "choices": [{"index": 0, "message": message, "finish_reason": finish}], "usage": usage}
                data = json.dumps(result).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Connection", "close")
            self.end_headers()
            base = {"id": "bench", "object": "chat.completion.chunk", "model": MODEL}

            def chunk(delta, reason=None):
                payload = dict(base, choices=[{"index": 0, "delta": delta, "finish_reason": reason}])
                self.wfile.write(b"data: " + json.dumps(payload).encode() + b"\n\n")

            chunk({"role": "assistant", "content": ""})
            if calls:
                for call in call_objects:
                    chunk({"tool_calls": [call]})
            else:
                # Fixed-size fragments make streaming work reproducible across runs.
                for pos in range(0, len(text), 256):
                    chunk({"content": text[pos:pos + 256]})
            chunk({}, finish)
            if (body.get("stream_options") or {}).get("include_usage"):
                self.wfile.write(b"data: " + json.dumps(dict(base, choices=[], usage=usage)).encode() + b"\n\n")
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
            self.close_connection = True
        except (BrokenPipeError, ConnectionResetError):
            self.close_connection = True


def setup(root, port):
    home, workspace = root / "home", root / "workspace"
    home.mkdir(); workspace.mkdir()
    credentials = home / ".octet" / "credentials"
    credentials.mkdir(parents=True, mode=0o700)
    config = {"version": 1, "providers": {"bench": {
        "label": "Bench", "base_url": f"http://127.0.0.1:{port}/v1/", "auth": {"kind": "none"},
        "auto_discover": False, "models": [{"api_name": MODEL, "context_window": 128000,
        "max_output_tokens": 8192, "tools": True, "parallel_tool_calls": True, "vision": False,
        "structured_output": False, "reasoning": False}]}}}
    path = credentials / "custom.json"
    path.write_text(json.dumps(config)); path.chmod(0o600)
    (home / ".octet" / "config.toml").write_text(f'model = "custom/bench/{MODEL}"\ntheme = "dark"\n')
    env = {"HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"),
           "XDG_CACHE_HOME": str(home / ".cache"), "XDG_DATA_HOME": str(home / ".local/share"),
           "XDG_STATE_HOME": str(home / ".local/state"), "PATH": "/usr/bin:/bin", "TERM": "xterm-256color",
           "LANG": "C.UTF-8", "OCTET_COLOR_SCHEME": "dark", "HTTP_PROXY": "http://127.0.0.1:9",
           "HTTPS_PROXY": "http://127.0.0.1:9", "ALL_PROXY": "http://127.0.0.1:9",
           "NO_PROXY": "127.0.0.1,localhost", "http_proxy": "http://127.0.0.1:9",
           "https_proxy": "http://127.0.0.1:9", "all_proxy": "http://127.0.0.1:9",
           "no_proxy": "127.0.0.1,localhost"}
    return home, workspace, env


def proc_sample(pid):
    """Own-process CPU ticks, RSS, and Linux private resident pages (not PSS)."""
    stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    cpu_ms = (int(stat[11]) + int(stat[12])) * 1000 / os.sysconf("SC_CLK_TCK")
    status = Path(f"/proc/{pid}/status").read_text()
    rss = int(re.search(r"^VmRSS:\s+(\d+)", status, re.M).group(1)) * 1024
    rollup = Path(f"/proc/{pid}/smaps_rollup").read_text()
    private = sum(int(value) * 1024 for value in re.findall(r"^Private_(?:Clean|Dirty|Hugetlb):\s+(\d+)", rollup, re.M))
    return {"cpu_ms": cpu_ms, "rss_bytes": rss, "private_bytes": private}


class PeakSampler:
    def __init__(self, pid):
        self.pid = pid
        self.peak_rss = 0
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        while not self.stop.is_set():
            try:
                text = Path(f"/proc/{self.pid}/status").read_text()
                match = re.search(r"^VmRSS:\s+(\d+)", text, re.M)
                if match:
                    self.peak_rss = max(self.peak_rss, int(match.group(1)) * 1024)
            except (FileNotFoundError, ProcessLookupError):
                break
            self.stop.wait(0.002)

    def close(self):
        self.stop.set(); self.thread.join()


def command(binary):
    return [str(binary), "--offline", "--no-context-files", "--workspace-trusted",
            "--model", f"custom/bench/{MODEL}"]


def verify_exchanges(scenario, exchanges, prompts=1):
    if scenario == "startup":
        if exchanges:
            raise AssertionError("provider called during startup")
        return
    expected = [0, 1, 2, 3, 4] if scenario == "tools" else ([0, 1] if scenario == "write" else [0] * prompts)
    actual = [item["tool_results"] for item in exchanges]
    if actual != expected:
        raise AssertionError(f"unexpected provider exchanges: {exchanges!r}")
    if scenario in ("interactive", "long") and [x["prompt"] for x in exchanges] != list(range(1, prompts + 1)):
        raise AssertionError(f"prompts missing, duplicated or reordered: {exchanges!r}")
    if scenario in ("tools", "write") and sum(x["tool_calls"] for x in exchanges) != len(expected) - 1:
        raise AssertionError("tool calls missing")
    if scenario == "tools":
        for item in exchanges:
            outputs = item.get("tool_outputs", [])
            if len(outputs) != item["tool_results"] or any(
                    f"step-{index}" not in output or "exit=0" not in output
                    for index, output in enumerate(outputs)):
                raise AssertionError("tool output missing or unsuccessful")
    if not all(item["stream"] for item in exchanges):
        raise AssertionError("non-streaming provider request")


def verify_stream_output(stdout):
    if stdout != STREAM_DATA.encode():
        raise AssertionError("stream output differs from 60,000-character fixture")


def headless(binary, server, scenario, timeout):
    server.reset(scenario)
    with tempfile.TemporaryDirectory(prefix="octet-bench-") as tmp:
        root = Path(tmp); _, workspace, env = setup(root, server.server_port)
        with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
            started = time.monotonic_ns()
            child = subprocess.Popen(command(binary) + ["--print", "BENCH_PROMPT_001"], cwd=workspace,
                                     env=env, stdin=subprocess.DEVNULL, stdout=output, stderr=errors)
            sampler = PeakSampler(child.pid)
            try:
                try:
                    waited_pid, status, usage = os.wait4(child.pid, os.WNOHANG)
                    deadline = time.monotonic() + timeout
                    while not waited_pid:
                        if time.monotonic() >= deadline:
                            child.kill(); os.wait4(child.pid, 0); child.returncode = -signal.SIGKILL
                            raise TimeoutError("headless process timed out")
                        time.sleep(0.002)
                        waited_pid, status, usage = os.wait4(child.pid, os.WNOHANG)
                    child.returncode = os.waitstatus_to_exitcode(status)
                finally:
                    sampler.close()
            finally:
                if child.returncode is None:
                    child.kill(); child.wait()
            wall_ms = (time.monotonic_ns() - started) / 1e6
            output.seek(0); stdout = output.read()
            errors.seek(0); stderr = errors.read()
            if child.returncode != 0:
                raise RuntimeError(f"headless exit {child.returncode}: {stderr[-3000:]!r}")
            if scenario == "stream":
                verify_stream_output(stdout)
            elif b"BENCH_DONE" not in stdout:
                raise AssertionError(f"answer missing: {stdout[-1000:]!r} {stderr[-1000:]!r}")
            if scenario == "write" and (workspace / "bench-output.txt").read_text() != FILE_DATA:
                raise AssertionError("written file differs from fixture")
            exchanges = server.snapshot()
            verify_exchanges(scenario, exchanges)
            return {"wall_ms": wall_ms, "cpu_ms": (usage.ru_utime + usage.ru_stime) * 1000,
                    "peak_rss_bytes": sampler.peak_rss,
                    "provider_exchanges": exchanges}


def interactive(binary, server, scenario, prompts, timeout):
    server.reset(scenario)
    with tempfile.TemporaryDirectory(prefix="octet-bench-") as tmp:
        root = Path(tmp); _, workspace, env = setup(root, server.server_port)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        original = termios.tcgetattr(master)

        def controlling_tty():
            os.setsid(); fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        with tempfile.TemporaryFile() as errors:
            start = time.monotonic_ns()
            try:
                child = subprocess.Popen(command(binary), cwd=workspace, env=env, stdin=slave, stdout=slave,
                                         stderr=errors, preexec_fn=controlling_tty)
            finally:
                os.close(slave)
            sampler = PeakSampler(child.pid)
            output = bytearray()
            try:
                def await_frame(marker):
                    tail = b""; seen = False; deadline = time.monotonic() + timeout
                    while time.monotonic() < deadline:
                        if child.poll() is not None:
                            errors.seek(0)
                            raise RuntimeError(f"interactive exit {child.returncode}: {errors.read()[-3000:]!r}; output={bytes(output[-1000:])!r}")
                        if not select.select([master], [], [], 0.01)[0]:
                            continue
                        data = os.read(master, 65536); output.extend(data)
                        scan = tail + data
                        if b"\x1b[6n" in scan and b"\x1b[6n" not in tail:
                            os.write(master, b"\x1b[1;1R")
                        pos = scan.find(marker)
                        if pos >= 0:
                            seen = True; scan = scan[pos:]
                        if seen and FRAME_END in scan:
                            return
                        tail = scan[-128:]
                    errors.seek(0)
                    raise TimeoutError(f"no frame for {marker!r}: {bytes(output[-1200:])!r}; stderr={errors.read()[-1200:]!r}")

                await_frame(MODEL.encode())
                ready_ms = (time.monotonic_ns() - start) / 1e6
                if termios.tcgetattr(master)[3] & (termios.ICANON | termios.ECHO):
                    raise AssertionError("composer not in raw mode")
                # Confirm the application, not terminal echo, accepts editing.
                os.write(master, b"EDITABLE_MARKER")
                await_frame(b"EDITABLE_MARKER")
                os.write(master, b"\x15")  # clear the proof text
                samples = []
                for index in range(1, prompts + 1):
                    before = proc_sample(child.pid)
                    started = time.monotonic_ns()
                    os.write(master, f"BENCH_PROMPT_{index:03}\r".encode())
                    await_frame(f"BENCH_REPLY_{index:03}".encode())
                    # Include completion bookkeeping and require quiet before another submit.
                    quiet = time.monotonic() + 0.03
                    while time.monotonic() < quiet:
                        if select.select([master], [], [], 0.002)[0]:
                            output.extend(os.read(master, 65536))
                            quiet = time.monotonic() + 0.03
                    after = proc_sample(child.pid)
                    samples.append({"prompt": index, "wall_ms": (time.monotonic_ns() - started) / 1e6,
                                    "cpu_ms": after["cpu_ms"] - before["cpu_ms"],
                                    "rss_bytes": after["rss_bytes"], "private_bytes": after["private_bytes"]})
                exchanges = server.snapshot()
                verify_exchanges(scenario, exchanges, prompts)
                os.write(master, b"\x04")
                deadline = time.monotonic() + timeout
                while child.poll() is None and time.monotonic() < deadline:
                    if select.select([master], [], [], 0.01)[0]:
                        try: os.read(master, 65536)
                        except OSError: pass
                if child.poll() is None:
                    raise TimeoutError("interactive shutdown timed out")
                if child.returncode != 0 or termios.tcgetattr(master) != original:
                    raise AssertionError("interactive shutdown failed or terminal modes not restored")
                return {"ready_ms": ready_ms, "peak_rss_bytes": sampler.peak_rss,
                        "prompts": samples, "provider_exchanges": exchanges}
            finally:
                sampler.close()
                if child.poll() is None:
                    child.kill(); child.wait()
                os.close(master)


def slope(samples):
    if len(samples) < 2:
        return None
    xs = [sample["prompt"] for sample in samples]
    ys = [sample["private_bytes"] for sample in samples]
    mx, my = statistics.mean(xs), statistics.mean(ys)
    return sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sum((x - mx) ** 2 for x in xs)


def summary(values):
    ordered = sorted(values)
    return {"median": statistics.median(ordered), "min": ordered[0], "max": ordered[-1]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--long-prompts", type=int, default=75)
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--scenarios", nargs="+", choices=["startup", "tools", "interactive", "long", "write", "stream"],
                        default=["startup", "tools", "interactive", "long", "write", "stream"])
    parser.add_argument("--max-long-slope-bytes", type=float, help="optional private-resident bytes/prompt gate")
    args = parser.parse_args()
    if args.runs < 1 or args.warmup < 0 or not 1 <= args.long_prompts <= 999 or args.timeout <= 0:
        parser.error("runs/timeout must be positive; warmup nonnegative; long prompts in 1..999")
    binary = args.binary.resolve(strict=True)
    if not Path("/proc/self/smaps_rollup").exists():
        parser.error("this runner requires Linux /proc and smaps_rollup")
    server = Provider()
    results = {"schema": "octet.session-bench.v1", "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
               "system": platform.platform(), "machine": platform.machine(), "clock_ticks_per_second": os.sysconf("SC_CLK_TCK"),
               "runs": args.runs, "warmup": args.warmup, "long_prompts": args.long_prompts,
               "memory_scope": "own process; private=Private_Clean+Private_Dirty+Private_Hugetlb from smaps_rollup; peak RSS sampled every 2ms (may miss brief peaks)",
               "cpu_scope": "interactive own process /proc ticks; headless wait4 user+system including waited-for children",
               "prompt_scope": "submission through answer frame and 30ms quiet; interactive UI, not plain mode",
               "allocation_counts": None, "instruction_counts": None, "scenarios": {}}
    try:
        for scenario in args.scenarios:
            runs = []
            for i in range(args.warmup + args.runs):
                count = 0 if scenario == "startup" else (5 if scenario == "interactive" else args.long_prompts)
                value = (interactive(binary, server, scenario, count, args.timeout)
                         if scenario in ("startup", "interactive", "long") else headless(binary, server, scenario, args.timeout))
                if i >= args.warmup:
                    runs.append(value)
                print(f"{scenario}: {i + 1}/{args.warmup + args.runs}", flush=True)
            keys = ["ready_ms", "peak_rss_bytes"] if scenario in ("startup", "interactive", "long") else ["wall_ms", "cpu_ms", "peak_rss_bytes"]
            row = {"samples": runs, "summary": {key: summary([run[key] for run in runs]) for key in keys}}
            if scenario == "long":
                slopes = [slope(run["prompts"]) for run in runs]
                if all(value is not None for value in slopes):
                    row["summary"]["private_slope_bytes_per_prompt"] = summary(slopes)
            results["scenarios"][scenario] = row
    finally:
        server.close()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(results, sort_keys=True, indent=2, allow_nan=False) + "\n")
    if args.max_long_slope_bytes is not None:
        metric = results["scenarios"].get("long", {}).get("summary", {}).get("private_slope_bytes_per_prompt")
        if metric is None or metric["median"] > args.max_long_slope_bytes:
            raise SystemExit("long-session private-memory slope gate failed or unavailable")


if __name__ == "__main__":
    main()

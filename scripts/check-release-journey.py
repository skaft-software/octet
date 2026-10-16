#!/usr/bin/env python3
"""Qualify supplied POSIX binaries with a disposable, loopback-only coding journey.

Does not build, install, publish, use credentials, or contact a live model.
Windows must use an isolated OS account: HOME does not relocate known folders.
"""
from __future__ import annotations

import argparse
from collections import deque
from contextlib import contextmanager
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import itertools
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import time

LIMIT = 1024 * 1024
WAIT = 20
MODEL = "fixture-model"
CALL_IDS = itertools.count(1)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def completion(text: str = "", *, tool: str | None = None, arguments: dict | None = None) -> bytes:
    delta: dict = {"role": "assistant", "content": text}
    if tool:
        delta["tool_calls"] = [{"index": 0, "id": f"journey-call-{next(CALL_IDS)}", "type": "function",
                                "function": {"name": tool, "arguments": json.dumps(arguments)}}]
    chunks = [
        {"choices": [{"index": 0, "delta": delta}]},
        {"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls" if tool else "stop"}],
         "usage": {"prompt_tokens": 20, "completion_tokens": 5, "total_tokens": 25}},
    ]
    return ("".join(f"data: {json.dumps(chunk)}\n\n" for chunk in chunks) + "data: [DONE]\n\n").encode()


def require_tool_result(request: dict, *markers: str) -> None:
    message = request["messages"][-1]
    require(message["role"] == "tool" and isinstance(message["content"], str)
            and all(marker in message["content"] for marker in markers),
            f"expected tool result text missing: {markers}")


class Provider:
    """Bounded HTTP fixture: every inference consumes one explicit scripted reply."""
    def __init__(self):
        self.replies: deque = deque()
        self.requests: list[dict] = []
        self.errors: list[str] = []
        self.lock = threading.Lock()
        self.entered = threading.Event()
        self.release = threading.Event()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_GET(self):
                if self.path != "/v1/models":
                    self.send_error(404)
                    return
                body = json.dumps({"data": [{"id": MODEL}]}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):
                self.connection.settimeout(5)
                length = int(self.headers.get("Content-Length", "0"))
                if self.path != "/v1/chat/completions" or not 0 < length <= LIMIT:
                    self.send_error(400)
                    return
                body = json.loads(self.rfile.read(length))
                with owner.lock:
                    owner.requests.append(body)
                    if not owner.replies:
                        owner.errors.append("unscripted provider request")
                        self.send_error(500)
                        return
                    reply = owner.replies.popleft()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                try:
                    if reply is None:
                        # An unfinished response after a *settled* mutation.
                        self.wfile.write(b'data: {"choices":[{"index":0,"delta":{"content":"unfinished"}}]}\n\n')
                        self.wfile.flush()
                        owner.entered.set()
                        owner.release.wait(WAIT)
                    else:
                        self.wfile.write(reply)
                        self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass  # Expected when the owned CLI is interrupted.

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/v1/"
        self.worker = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.worker.start()

    def enqueue(self, *replies: bytes | None) -> None:
        with self.lock:
            self.replies.extend(replies)

    def close(self) -> None:
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.worker.join(timeout=5)


class Child:
    def __init__(self, command: list[str], environment: dict[str, str], cwd: Path, payload: bytes | None):
        self.stdout = tempfile.TemporaryFile()
        self.stderr = tempfile.TemporaryFile()
        self.process = subprocess.Popen(command, cwd=cwd, env=environment, start_new_session=True,
                                        stdin=subprocess.PIPE if payload is not None else subprocess.DEVNULL,
                                        stdout=self.stdout, stderr=self.stderr)
        if payload is not None:
            self.process.stdin.write(payload)
            self.process.stdin.close()

    def bounded(self) -> None:
        require(all(os.fstat(stream.fileno()).st_size <= LIMIT for stream in (self.stdout, self.stderr)),
                "child output exceeded 1 MiB")

    def wait_until(self, ready) -> None:
        deadline = time.monotonic() + WAIT
        while not ready():
            self.bounded()
            if self.process.poll() is not None:
                raise RuntimeError(f"child exited before fixture boundary: {self.result()}")
            require(time.monotonic() < deadline, "child did not reach fixture boundary")
            time.sleep(0.02)

    def result(self, *, success: bool = False) -> tuple[int, str, str]:
        deadline = time.monotonic() + WAIT
        while self.process.poll() is None:
            self.bounded()
            require(time.monotonic() < deadline, "child exit timed out")
            time.sleep(0.02)
        self.bounded()
        streams = []
        for stream in (self.stdout, self.stderr):
            stream.seek(0)
            streams.append(stream.read(LIMIT + 1).decode("utf-8", errors="replace"))
        if success:
            require(self.process.returncode == 0, f"child failed: {streams[1]}")
        return self.process.returncode, *streams

    def stop(self, sig: int) -> None:
        if self.process.poll() is None:
            os.killpg(self.process.pid, sig)  # Only the session this harness created.

    def close(self) -> None:
        self.stop(signal.SIGKILL)
        self.process.wait(timeout=5)
        self.stdout.close()
        self.stderr.close()


@contextmanager
def child(command: list[str], environment: dict[str, str], cwd: Path, payload: bytes | None = None):
    running = Child(command, environment, cwd, payload)
    try:
        yield running
    finally:
        running.close()


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        checksum = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            checksum.update(block)
        return checksum.hexdigest()


def qualify(binary: Path, host: Path, version: str, report: dict) -> None:
    with tempfile.TemporaryDirectory(prefix="octet-release-journey-") as temporary:
        root = Path(temporary).resolve()
        home, workspace, sessions = (root / name for name in ("home", "workspace", "sessions"))
        for directory in (home, workspace, sessions):
            directory.mkdir(mode=0o700)
        environment = {"HOME": str(home), "PATH": os.defpath, "LANG": "C.UTF-8", "TERM": "dumb",
                       "NO_COLOR": "1", "OCTET_CACHE_WARMING": "off", "AWS_EC2_METADATA_DISABLED": "true"}
        common = [str(binary), "--workspace", str(workspace), "--session-dir", str(sessions),
                  "--no-context-files", "--color", "never", "--cache-warming", "off"]

        def invoke(command: list[str], payload: bytes | None = None) -> str:
            with child(command, environment, workspace, payload) as running:
                return running.result(success=True)[1]

        require(invoke([str(binary), "--version"]).strip() == f"octet {version}", "binary version mismatch")
        invoke([str(binary), "--help"])
        hello = json.loads(invoke([str(host)], b'{"protocol_version":1,"request_id":"journey","command":"hello"}\n'))
        require(hello["type"] == "hello" and hello["data"]["sdk_version"] == version
                and hello["data"]["protocol_version"] == 1, "host handshake mismatch")
        report["passed"].append("exact_binary_version_and_host_handshake")

        provider = Provider()
        try:
            invoke(common + ["setup", "--endpoint", provider.url, "--provider", "fixture", "--model", MODEL,
                             "--context-window", "32768", "--no-auth", "--yes"])
            require((home / ".octet/credentials/custom.json").is_file(), "setup did not persist isolated provider")
            run = common + ["--offline", "--model", f"custom/fixture/{MODEL}", "--effect-policy", "unsafe_host",
                            "--tools", "read,edit,write,bash", "--max-turns", "8"]
            original = b"one\r\ntwo\r\n"
            original_hash = hashlib.sha256(original).hexdigest()
            edited = b"ONE\r\nTWO\r\nTHREE\r\n"
            (workspace / "lines.txt").write_bytes(original)
            provider.enqueue(
                completion(tool="read", arguments={"path": "lines.txt"}),
                completion(tool="edit", arguments={"path": "lines.txt", "oldText": "one\ntwo",
                           "newText": "ONE\nTWO\nTHREE", "expected_hash": original_hash}),
                completion(tool="bash", arguments={"command": "printf fixture-shell-error >&2; exit 7"}),
                completion("JOURNEY_CODING_DONE"),
            )
            require("JOURNEY_CODING_DONE" in invoke(run + ["--print", "coding fixture"]), "coding run did not finish")
            require((workspace / "lines.txt").read_bytes() == edited, "CRLF edit bytes changed")
            require_tool_result(provider.requests[1], "1: one\n2: two\n", f"hash={original_hash}")
            require_tool_result(provider.requests[2], "ok modified=1", f"hash={hashlib.sha256(edited).hexdigest()}")
            require_tool_result(provider.requests[3], "exit=7")
            report["passed"].append("provider_setup_read_edit_crlf_and_shell_failure")

            # Both commands are disposable and bounded even if cancellation regresses.
            provider.enqueue(
                completion(tool="bash", arguments={"command": "printf 'once\\n' >> mutations.log"}),
                completion(tool="bash", arguments={"command": "printf '%s' $$ > shell-started; sleep 30"}),
            )
            with child(run + ["--print", "cancellation fixture"], environment, workspace) as running:
                running.wait_until(lambda: (workspace / "shell-started").exists())
                running.stop(signal.SIGINT)
                running.result()
            # Probe only the PID written by our fixture. Never signal an
            # unrelated process or infer cancellation from CLI exit alone.
            shell_pid = int((workspace / "shell-started").read_text())
            deadline = time.monotonic() + 3
            while True:
                try:
                    os.kill(shell_pid, 0)
                except ProcessLookupError:
                    break
                require(time.monotonic() < deadline, "cancelled shell was not reaped")
                time.sleep(0.02)
            provider.enqueue(completion("JOURNEY_RESUMED"))
            require("JOURNEY_RESUMED" in invoke(run + ["--continue", "--print", "resume cancelled fixture"]),
                    "cancelled session did not resume")
            require((workspace / "mutations.log").read_bytes() == b"once\n", "completed mutation was duplicated on resume")
            report["passed"].append("shell_cancellation_and_resume_without_duplicate_mutation")

            provider.enqueue(
                completion(tool="bash", arguments={"command": "printf 'crash-once\\n' >> crash-mutations.log"}), None,
            )
            with child(run + ["--print", "abrupt interruption fixture"], environment, workspace) as running:
                running.wait_until(provider.entered.is_set)
                running.stop(signal.SIGKILL)
                running.result()
            provider.release.set()
            provider.enqueue(completion("JOURNEY_CRASH_RESUMED"))
            require("JOURNEY_CRASH_RESUMED" in invoke(run + ["--continue", "--print", "resume interrupted fixture"]),
                    "interrupted session did not resume")
            require((workspace / "crash-mutations.log").read_bytes() == b"crash-once\n", "settled mutation replayed after crash")
            require(not provider.errors and not provider.replies, f"fixture mismatch: {provider.errors}")
            report["passed"].append("abrupt_interruption_and_resume_of_settled_mutation")
        finally:
            provider.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--host-binary", type=Path, required=True)
    parser.add_argument("--expected-version", required=True)
    parser.add_argument("--report", type=Path, help="write binary digests and observed results to this JSON path")
    args = parser.parse_args()
    if os.name != "posix":
        parser.error("POSIX-only: Windows HOME does not isolate the native profile; use Windows native gates and an isolated OS account")
    binary, host = args.binary.resolve(strict=True), args.host_binary.resolve(strict=True)
    # WSL can execute PE files despite os.name == 'posix'. Do not let that
    # bypass native-profile isolation; this lane accepts native ELF/Mach-O only.
    native_magic = {b'\x7fELF', b'\xcf\xfa\xed\xfe', b'\xfe\xed\xfa\xcf',
                    b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca', b'\xca\xfe\xba\xbf', b'\xbf\xba\xfe\xca'}
    for candidate in (binary, host):
        with candidate.open("rb") as stream:
            if stream.read(4) not in native_magic:
                parser.error("requires native ELF/Mach-O binaries, not Windows PE executables or launcher scripts")
    report = {"schema": "octet.release-journey.v1", "version": args.expected_version,
              "binaries": {"octet": digest(binary), "octet-host": digest(host)}, "passed": [],
              "not_run": ["live_provider", "attended_desktop", "ambiguous_mutation_crash_window"], "status": "failed"}
    try:
        qualify(binary, host, args.expected_version, report)
        report["status"] = "passed"
    except (OSError, RuntimeError, ValueError, KeyError, subprocess.SubprocessError) as error:
        report["error"] = str(error)
    output = json.dumps(report, indent=2) + "\n"
    if args.report:
        args.report.write_text(output, encoding="utf-8")
    print(output, end="")
    return int(report["status"] != "passed")


if __name__ == "__main__":
    raise SystemExit(main())

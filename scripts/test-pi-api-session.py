#!/usr/bin/env python3
"""Bounded real Octet/PTY + real Pi adapter checkpoint for Pi 1.0.2 rows 1-4.

Only inference is scripted, on a loopback OpenAI chat SSE server. No builds,
installs, real credentials, global HOME changes, or product monkeypatching.
Run: python3 -B scripts/test-pi-api-session.py --binary target/debug/octet --keep
The compact JSON on stdout is the result; exit 1 means any check failed.
--keep retains private PTY, provider requests, and native session JSONL evidence.
A stale binary is reported, never presented as acceptance of current Rust source.
This is a checkpoint for named cases, not qualification of every member in a row.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("pi_pty", ROOT / "scripts/test-pi-compat-pty.py")
PTY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PTY)
FIXTURE = ROOT / "scripts/fixtures/pi-api-session/index.ts"
CUSTOM = "custom-probe"
DETAILS = "probe-details-sentinel"
CASES = {
    "CHECKPOINT-READ": ("read-success", "original.txt"),
    "CHECKPOINT-ERROR": ("read-error", "missing-checkpoint.txt"),
    "CHECKPOINT-DENY": ("policy-deny", "policy-original.txt"),
    "CHECKPOINT-ENTER": ("policy-enter", "../policy-enter.txt"),
}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def wire_chunks(delta, finish):
    return [{"id": "checkpoint", "object": "chat.completion.chunk", "model": "probe",
             "choices": [{"index": 0, "delta": {"role": "assistant", **delta}, "finish_reason": None}]},
            {"id": "checkpoint", "object": "chat.completion.chunk", "model": "probe",
             "choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
             "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}]


class Provider:
    """Record bounded actual HTTP bodies, respond only to the fixture's script."""
    def __init__(self):
        self.requests = []
        self.errors = []
        self.lock = threading.Lock()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_GET(self):
                with owner.lock:
                    owner.errors.append(f"unexpected GET {self.path}")
                self.send_error(404)

            def do_POST(self):
                try:
                    n = int(self.headers.get("Content-Length", "0"))
                    if not 0 < n <= 2 * 1024 * 1024:
                        raise ValueError("provider body exceeds bound")
                    body = json.loads(self.rfile.read(n))
                    if self.path != "/v1/chat/completions" or body.get("stream") is not True:
                        raise ValueError("expected streaming OpenAI chat request")
                    with owner.lock:
                        if len(owner.requests) >= 32:
                            raise ValueError("provider request bound exceeded")
                        owner.requests.append({"path": self.path, "body": body})
                    messages = body["messages"]
                    last = messages[-1]
                    if last.get("role") == "tool":
                        call_id = last["tool_call_id"]
                        if call_id not in {c[0] for c in CASES.values()}:
                            raise ValueError(f"unexpected tool result {call_id}")
                        chunks = wire_chunks({"content": f"checkpoint-done-{call_id}"}, "stop")
                    elif last.get("role") == "user":
                        text = json.dumps(last.get("content"))
                        case = next((c for mark, c in CASES.items() if mark in text), None)
                        if case:
                            call_id, path = case
                            chunks = wire_chunks({"tool_calls": [{"index": 0, "id": call_id,
                                "type": "function", "function": {"name": "read",
                                "arguments": json.dumps({"path": path})}}]}, "tool_calls")
                        elif CUSTOM in text or last.get("content") == "":
                            # Complete even an empty surrogate from a broken host;
                            # assertions below still require the actual custom body.
                            # A 400 here would obscure the product's data-loss bug.
                            chunks = wire_chunks({"content": "checkpoint-idle-done"}, "stop")
                        elif "CHECKPOINT-RESUME" in text:
                            chunks = wire_chunks({"content": "checkpoint-resume-done"}, "stop")
                        elif "CHECKPOINT-FRESH" in text:
                            chunks = wire_chunks({"content": "checkpoint-fresh-done"}, "stop")
                        else:
                            raise ValueError("unexpected user request (script does not infer)")
                    else:
                        raise ValueError(f"unexpected last role {last.get('role')}")
                    data = b"".join(b"data: " + json.dumps(c).encode() + b"\n\n" for c in chunks)
                    data += b"data: [DONE]\n\n"
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(data)))
                    self.send_header("Connection", "close")
                    self.end_headers()
                    self.wfile.write(data)
                except (ValueError, KeyError, OSError) as error:
                    with owner.lock:
                        owner.errors.append(str(error))
                    self.send_error(400)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server.server_address[1]}/v1/"

    def snapshot(self):
        with self.lock:
            return list(self.requests)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def walk(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from walk(child)
    elif isinstance(value, list):
        for child in value:
            yield from walk(child)


def session_records(directory):
    records = []
    for path in sorted(directory.rglob("*.jsonl")):
        if path.stat().st_size > 4 * 1024 * 1024:
            raise AssertionError("session evidence exceeds 4MiB bound")
        for line in path.read_text().splitlines():
            records.append((path, json.loads(line)))
    return records


def text_content_matches(actual, expected):
    # Native/OpenAI user projections can encode one string as a text block.
    # Do not join blocks: their order/boundaries and exact text remain evidence.
    def parts(value):
        if isinstance(value, str):
            return [{"type": "text", "text": value}]
        if isinstance(value, list) and all(isinstance(part, dict)
                and set(part) == {"type", "text"} and part["type"] == "text"
                and isinstance(part["text"], str) for part in value):
            return value
        return None
    actual_parts, expected_parts = parts(actual), parts(expected)
    return actual_parts is not None and expected_parts is not None and actual_parts == expected_parts


def custom_entries(directory, content):
    # Require the canonical native typed metadata location. Neither ordinary
    # user text nor a lookalike object nested in inert details proves persistence.
    result = []
    for path, record in session_records(directory):
        if record.get("type") != "entry":
            continue
        node = (record.get("metadata") or {}).get("custom_message")
        if isinstance(node, dict) and node.get("custom_type") == "probe" and text_content_matches(node.get("content"), content):
            result.append((path, node))
    return result


def tool_results(directory, call_id):
    return [node["ToolResult"] for _, record in session_records(directory)
            if record.get("type") == "entry" for node in walk(record)
            if isinstance(node.get("ToolResult"), dict)
            and node["ToolResult"].get("tool_call_id") == call_id]


def model_results(provider, call_id):
    return [m for request in provider.snapshot() for m in request["body"]["messages"]
            if m.get("role") == "tool" and m.get("tool_call_id") == call_id]


class Checkpoint:
    def __init__(self, args, root, provider, checks):
        self.args, self.root, self.provider, self.checks = args, root, provider, checks
        self.env, self.workspace = PTY.isolated(root)
        self.env.update({"OCTET_TERN": "off", "OCTET_ALLOW_EXTERNAL_PATHS": "false"})
        credentials = root / "home/.octet/credentials/custom.json"
        data = json.loads(credentials.read_text())
        data["base_url"] = provider.url
        credentials.write_text(json.dumps(data))
        for name, text in {"original.txt": "ORIGINAL-TOKEN", "changed.txt": "CHANGED-TOKEN",
                           "policy-original.txt": "POLICY-ORIGINAL-TOKEN"}.items():
            (self.workspace / name).write_text(text + "\n")
        for name in ("policy-denied.txt", "policy-enter.txt"):
            (root / name).write_text("OUTSIDE-SECRET-TOKEN\n")
        self.extensions = PTY.configure(args.adapter, args.node, root, [FIXTURE], self.env, self.workspace)
        self.sessions = root / "sessions"
        self.terminal = None
        self.number = 0

    def check(self, row, name, predicate, evidence):
        self.checks.append({"row": row, "name": name, "status": "PASS" if predicate else "FAIL",
                            "evidence": str(evidence)[:1200]})

    def step(self, row, name, action):
        try:
            action()
            return True
        except (AssertionError, OSError, ValueError, subprocess.SubprocessError) as error:
            self.check(row, name, False, error)
            return False

    def start(self, policy="unsafe_host", resume=None):
        command = [a for a in PTY.native_command(self.args.binary, self.extensions) if a != "--no-tools"]
        command[command.index("--effect-policy") + 1] = policy
        command += ["--session-dir", str(self.sessions)]
        if resume:
            command += ["--resume", resume.stem]
        self.number += 1
        self.terminal = PTY.TerminalProcess(command, self.env, self.workspace, columns=140, rows=48)
        self.wait(lambda: "custom/probe" in self.terminal.screen.text(), "native ready")
        # Model footer can precede the final startup owner/snapshot delivery.
        self.terminal.pump(0.3)

    def wait(self, predicate, label):
        self.terminal.until(lambda _s: predicate(), timeout=self.args.timeout, label=label)

    def prompt(self, marker, done):
        self.terminal.send((marker + "\r").encode())
        self.wait(lambda: done in self.terminal.screen.text(), done)
        self.terminal.pump(0.15)

    def command(self, name):
        # Native Octet routes extension commands through /extensions, not removed
        # slash aliases. This still executes the fixture's real Pi command handler.
        t = self.terminal
        t.send(b"/extensions")
        t.pump(0.15)
        t.send(b"\x1b")  # dismiss completion without clearing the editor
        t.pump(0.1)
        t.send(b"\r")
        self.wait(lambda: "Manage extensions" in t.screen.text(), "extension manager")
        t.send(b"octet-pi-compat\r")
        self.wait(lambda: "probe-message" in t.screen.text(), "fixture commands")
        t.send(name.encode())
        t.pump(0.1)
        t.send(b"\r")
        self.wait(lambda: f"Arguments for {name}" in t.screen.text(), f"{name} arguments")
        t.send(b"\r")
        self.wait(lambda: "Arguments for" not in t.screen.text() and
                  ("Filter  type to filter" in t.screen.text()
                   or "extension session create completed" in t.screen.text()), f"{name} settled")
        # Replacement returns directly to idle. Ordinary commands return to the
        # pickers; leave both so the idle loop can consume custom messages.
        if "Filter  type to filter" in t.screen.text():
            t.send(b"\x1b"); t.pump(0.15)
            t.send(b"\x1b"); t.pump(0.15)

    def stop(self):
        if not self.terminal:
            return
        t, self.terminal = self.terminal, None
        try:
            t.send(b"\x03"); t.pump(0.1)  # empty composer / dismiss a failed picker
            t.close()
            self.check(0, f"shutdown-{self.root.name}-{self.number}", True, "Ctrl+D exit=0; termios/bracketed paste restored")
        except (AssertionError, OSError, subprocess.SubprocessError) as error:
            self.check(0, f"shutdown-{self.root.name}-{self.number}", False, error)
        finally:
            (self.root / f"terminal-{self.number}.pty").write_bytes(t.capture)
            (self.root / f"screen-{self.number}.txt").write_text(t.screen.text())
            diagnostics = re.findall(r"Pi compatibility error:[^\r\n\x1b]*", t.capture.decode(errors="replace"))
            self.check(0, f"adapter-diagnostics-{self.root.name}-{self.number}", not diagnostics,
                       "; ".join(dict.fromkeys(diagnostics)) or "no Pi compatibility errors in PTY output")
            t.dispose()

    def ordinary_tools(self):
        self.prompt("CHECKPOINT-READ", "checkpoint-done-read-success")
        messages = model_results(self.provider, "read-success")
        text = json.dumps(messages)
        self.check(1, "mutated-arguments-executed", bool(messages) and "CHANGED-TOKEN" in text
                   and "ORIGINAL-TOKEN" not in text, text)
        self.check(2, "actual-result-replacement-reaches-model", bool(messages)
                   and "CHANGED-TOKEN" in text and "[replaced input.path=changed.txt seenError=false]" in text, text)
        self.prompt("CHECKPOINT-ERROR", "checkpoint-done-read-error")
        messages = model_results(self.provider, "read-error")
        persisted = tool_results(self.sessions, "read-error")
        text = json.dumps(messages)
        self.check(2, "execution-error-survives-replacement", bool(messages) and len(persisted) == 1
                   and persisted[0].get("is_error") is True
                   and "[replaced input.path=missing-checkpoint.txt seenError=true]" in text,
                   {"model": messages, "native": persisted})

    def idle_message(self):
        before = len(self.provider.snapshot())
        self.command("probe-message")
        self.wait(lambda: len(self.provider.snapshot()) > before, "idle custom message wakes model")
        self.wait(lambda: "checkpoint-idle-done" in self.terminal.screen.text(), "idle custom completion")
        requests = self.provider.snapshot()[before:]
        messages = [m for r in requests for m in r["body"]["messages"]
                    if m.get("role") == "user" and CUSTOM in json.dumps(m.get("content"))]
        self.check(3, "idle-wake-and-private-details", bool(messages) and len(requests) == 1
                   and requests[0]["body"]["messages"][-1].get("role") == "user"
                   and text_content_matches(requests[0]["body"]["messages"][-1].get("content"), CUSTOM)
                   and all(DETAILS not in json.dumps(r["body"]) for r in requests),
                   [r["body"]["messages"][-1] for r in requests])
        entries = custom_entries(self.sessions, CUSTOM)
        self.check(3, "typed-custom-message-persistence", len(entries) == 1
                   and entries[0][1].get("display") is False
                   and entries[0][1].get("details") == {"n": 1, "sentinel": DETAILS}, entries)
        # Replay every emitted frame: checking only the last screen could miss a
        # hidden message that flashed briefly or scrolled out of the viewport.
        visible = displayed(self.terminal, CUSTOM)
        self.check(3, "hidden-live-display", bool(messages) and len(entries) == 1 and not visible,
                   f"model_messages={len(messages)} typed_entries={len(entries)} token_visible_in_any_frame={visible}")

    def resume_hidden(self):
        entries = custom_entries(self.sessions, CUSTOM)
        if len(entries) != 1:
            self.check(3, "hidden-resume-display", False, "blocked: no unique typed custom message to resume")
            self.check(3, "resumed-model-projection", False, "blocked: typed custom persistence missing")
            return
        session = entries[0][0]
        self.stop()
        self.start(resume=session)
        visible = displayed(self.terminal, CUSTOM)
        self.check(3, "hidden-resume-display", not visible, f"token_visible_in_any_resume_frame={visible}")
        before = len(self.provider.snapshot())
        self.prompt("CHECKPOINT-RESUME", "checkpoint-resume-done")
        requests = self.provider.snapshot()[before:]
        user_messages = [m for r in requests for m in r["body"]["messages"] if m.get("role") == "user"]
        self.check(3, "resumed-model-projection", CUSTOM in json.dumps(user_messages)
                   and all(DETAILS not in json.dumps(r["body"]) for r in requests), user_messages)

    def new_session(self):
        self.terminal.pump(0.2)
        old = {p: p.read_bytes() for p in self.sessions.rglob("*.jsonl")}
        before = len(self.provider.snapshot())
        self.command("probe-new")
        if self.step(4, "fresh-context-notification", lambda: self.wait(
                lambda: "fresh-rpc-complete " in self.terminal.screen.text(), "fresh context after two RPC awaits")):
            self.check(4, "fresh-context-notification", True, "fresh-session notification observed after two native RPC writes")
        first = custom_entries(self.sessions, "in-new-session")
        second = custom_entries(self.sessions, "after-fresh-rpc")
        new_files = set(self.sessions.rglob("*.jsonl")) - set(old)
        good = (len(new_files) == 1 and len(first) == len(second) == 1
                and first[0][0] == second[0][0] == next(iter(new_files)))
        session_id = first[0][1].get("details", {}).get("sessionId") if first else None
        self.check(4, "replacement-owner-survives-rpc", good and bool(session_id)
                   and first[0][0].stem == session_id
                   and first[0][1].get("display") is True and second[0][1].get("display") is True
                   and first[0][1].get("details") == {"sessionId": session_id, "sentinel": "fresh-details-sentinel"}
                   and second[0][1].get("details") == {"sessionId": session_id}
                   and f"fresh-rpc-complete {session_id}" in self.terminal.screen.text(), [first, second])
        error_visible = displayed(self.terminal, "probe-new:")
        self.check(4, "replacement-command-no-error", good and not error_visible,
                   f"typed_fresh_writes={len(first) + len(second)} command_error_visible={error_visible}")
        unchanged = bool(old) and all(p.read_bytes() == b for p, b in old.items())
        self.check(4, "old-session-byte-stable", len(new_files) == 1 and unchanged,
                   f"new_files={len(new_files)} old_JSONL_bytes_unchanged={unchanged}")
        self.check(4, "default-message-does-not-trigger-turn", good and len(self.provider.snapshot()) == before,
                   f"provider requests before={before} after={len(self.provider.snapshot())}")
        if not self.step(4, "replacement-model-turn", lambda: self.prompt("CHECKPOINT-FRESH", "checkpoint-fresh-done")):
            self.check(4, "new-model-context-is-fresh", False, "blocked: replacement session did not complete a model turn")
            self.check(4, "old-session-byte-stable-after-new-turn", False, "blocked: replacement model turn failed")
            return
        requests = self.provider.snapshot()[before:]
        text = json.dumps([r["body"] for r in requests])
        self.check(4, "new-model-context-is-fresh", bool(requests) and "in-new-session" in text
                   and "after-fresh-rpc" in text and "CHECKPOINT-READ" not in text and CUSTOM not in text
                   and "fresh-details-sentinel" not in text, text[-1000:])
        unchanged = bool(old) and all(p.read_bytes() == b for p, b in old.items())
        self.check(4, "old-session-byte-stable-after-new-turn", good and unchanged,
                   f"old_JSONL_bytes_unchanged_after_new_model_turn={unchanged}")

    def policy(self):
        self.prompt("CHECKPOINT-DENY", "checkpoint-done-policy-deny")
        messages = model_results(self.provider, "policy-deny")
        persisted = tool_results(self.sessions, "policy-deny")
        text = json.dumps(messages)
        self.check(1, "mutated-path-policy-denial", bool(messages) and len(persisted) == 1
                   and persisted[0].get("is_error") is True and "OUTSIDE-SECRET-TOKEN" not in text
                   and "POLICY-ORIGINAL-TOKEN" not in text and "policy-denied.txt" in text
                   and "absolute paths are not allowed" in text,
                   {"model": messages, "native": persisted})
        self.check(2, "replacement-cannot-clear-policy-error", len(persisted) == 1
                   and persisted[0].get("is_error") is True, persisted)
        self.prompt("CHECKPOINT-ENTER", "checkpoint-done-policy-enter")
        messages = model_results(self.provider, "policy-enter")
        text = json.dumps(messages)
        self.check(1, "mutation-before-policy-admission", bool(messages) and "CHANGED-TOKEN" in text
                   and "OUTSIDE-SECRET-TOKEN" not in text
                   and "[replaced input.path=changed.txt seenError=false]" in text, messages)


def displayed(terminal, token):
    screen = PTY.Screen(terminal.screen.columns, terminal.screen.rows)
    offset = 0
    for frame in terminal.clock.frames:
        screen.feed(terminal.capture[offset:frame["end"]])
        offset = frame["end"]
        if token in screen.text():
            return True
    screen.feed(terminal.capture[offset:])
    return token in screen.text()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/octet")
    parser.add_argument("--adapter", type=Path, default=ROOT / "extensions/octet-pi-compat")
    parser.add_argument("--node", type=Path, default=Path(shutil.which("node") or "/missing-node"))
    parser.add_argument("--timeout", type=float, default=12, help="per-condition bound, seconds (1..30)")
    parser.add_argument("--keep", action="store_true", help="retain private temporary evidence")
    args = parser.parse_args()
    if os.name != "posix" or not 1 <= args.timeout <= 30:
        parser.error("requires Unix PTY and --timeout 1..30")
    args.binary, args.adapter, args.node = args.binary.resolve(), args.adapter.resolve(), args.node.resolve()
    for path in (args.binary, args.node, args.adapter / "configure.mjs"):
        if not path.is_file():
            parser.error(f"missing prerequisite: {path}")
    root = Path(tempfile.mkdtemp(prefix="octet-pi-api-session-")).resolve()
    root.chmod(0o700)
    checks = []
    stamp = args.binary.stat().st_mtime_ns
    report = {"binary": str(args.binary), "binary_sha256": digest(args.binary),
              "binary_mtime_ns": stamp,
              "source_newer_than_binary": any(p.stat().st_mtime_ns > stamp for p in
                  (ROOT / "crates/octet-agent/src/session.rs",
                   ROOT / "crates/octet-coding-agent/src/modes/interactive.rs",
                   ROOT / "crates/octet-coding-agent/src/extensions/host_requests.rs")),
              "checks": checks, "evidence_dir": str(root) if args.keep else None}
    provider = Provider()
    try:
        for name in ("main", "policy"):
            directory = root / name
            directory.mkdir()
            checkpoint = None
            try:
                checkpoint = Checkpoint(args, directory, provider, checks)
                checkpoint.start("controlled" if name == "policy" else "unsafe_host")
                if name == "policy":
                    checkpoint.step(1, "policy-path", checkpoint.policy)
                else:
                    checkpoint.step(1, "tools-path", checkpoint.ordinary_tools)
                    checkpoint.step(3, "idle-message-path", checkpoint.idle_message)
                    checkpoint.step(3, "resume-path", checkpoint.resume_hidden)
                    checkpoint.step(4, "new-session-path", checkpoint.new_session)
            except (AssertionError, OSError, ValueError, subprocess.SubprocessError) as error:
                checks.append({"row": 0, "name": f"{name}-startup", "status": "FAIL", "evidence": str(error)[:1200]})
            finally:
                if checkpoint:
                    checkpoint.stop()
        checks.append({"row": 0, "name": "scripted-provider", "status": "FAIL" if provider.errors else "PASS",
                       "evidence": provider.errors or f"{len(provider.snapshot())} loopback requests, scripted SSE only"})
        checks.append({"row": 0, "name": "binary-unchanged-during-run",
                       "status": "PASS" if digest(args.binary) == report["binary_sha256"] else "FAIL",
                       "evidence": "binary hash compared before/after checkpoint"})
    finally:
        provider.close()
        (root / "requests.json").write_text(json.dumps(provider.snapshot(), indent=2) + "\n")
        (root / "result.json").write_text(json.dumps(report, default=str, indent=2) + "\n")
        print(json.dumps(report, default=str, separators=(",", ":")))
        if not args.keep:
            shutil.rmtree(root)
    return 0 if checks and all(c["status"] == "PASS" for c in checks) else 1


if __name__ == "__main__":
    sys.exit(main())

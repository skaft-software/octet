#!/usr/bin/env python3
"""Acceptance test: the word-count examples against the real octet host.

Both examples register one model tool (`word_count`) and one slash command
(`wordcount`). This test copies each example into a scratch extension directory,
serves a scripted OpenAI-compatible endpoint that answers with a `word_count`
tool call, and drives the real binary (`OCTET_BIN`, default `bin/octet-latest`)
in print mode so the extension process must actually run. It then types
`/wordcount sample.txt` into the real TUI on a PTY.

No paid model, network or publish step is used. Run it from the repository
root after building a host:

    python3 examples/extensions/word_count_examples_test.py
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
EXAMPLES = REPO / "examples" / "extensions"
SAMPLE = (
    "The quick brown fox jumps over the lazy dog.\n"
    "Extensions are subprocesses speaking JSON-RPC on stdin and stdout.\n"
)
EXPECTED = "sample.txt: 18 words"

sys.path.insert(0, str(REPO / "scripts" / "e2e"))
try:
    from octet_e2e.session import OctetSession
except ImportError:  # the documented PTY harness is part of the checkout
    OctetSession = None


def find_binary() -> Path | None:
    candidates = [
        os.environ.get("OCTET_BIN"),
        REPO / "bin" / "octet-latest",
        REPO / "target" / "debug" / "octet",
        REPO / "target" / "release" / "octet",
    ]
    for candidate in candidates:
        if candidate and Path(candidate).is_file():
            return Path(candidate)
    return None


class ScriptedProvider:
    """Loopback OpenAI-compatible endpoint that answers with a word_count call."""

    def __init__(self):
        self.tool_results: list[str] = []
        self.errors: list[str] = []
        provider = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):
                pass

            def _send_json(self, payload: dict):
                body = json.dumps(payload).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                self._send_json({"object": "list", "data": [{"id": "mock-1", "object": "model"}]})

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))) or b"{}")
                last = body.get("messages", [{}])[-1]
                if last.get("role") == "tool":
                    result = str(last.get("content"))
                    provider.tool_results.append(result)
                    delta, finish = {"content": f"word count result: {result}"}, "stop"
                else:
                    names = [tool["function"]["name"] for tool in body.get("tools", [])]
                    if "word_count" not in names:
                        provider.errors.append(f"word_count not advertised: {names}")
                    delta = {"tool_calls": [{
                        "index": 0, "id": "call-1", "type": "function",
                        "function": {"name": "word_count", "arguments": "{\"path\":\"sample.txt\"}"}}]}
                    finish = "tool_calls"
                chunk = json.dumps({"id": "fixture", "choices": [{"delta": delta}]})
                final = json.dumps({"id": "fixture", "choices": [{"delta": {}, "finish_reason": finish}]})
                payload = f"data: {chunk}\n\ndata: {final}\n\ndata: [DONE]\n\n".encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.port = self.server.server_address[1]

    def close(self):
        self.server.shutdown()
        self.server.server_close()


def make_env(root: Path, port: int, name: str) -> dict:
    home, workspace, sessions = root / "home", root / "workspace", root / "sessions"
    (home / ".octet" / "credentials").mkdir(parents=True)
    workspace.mkdir(parents=True)
    sessions.mkdir(parents=True)
    (workspace / "sample.txt").write_text(SAMPLE)
    registry = {"version": 1, "providers": {"scripted": {
        "label": "Scripted word count",
        "base_url": f"http://127.0.0.1:{port}/v1/",
        "auth": {"kind": "none"},
        "auto_discover": False,
        "models": [{"api_name": "mock-1", "context_window": 32768,
                    "max_output_tokens": 4096, "tools": True,
                    "parallel_tool_calls": False}],
    }}}
    credential = home / ".octet" / "credentials" / "custom.json"
    credential.write_text(json.dumps(registry))
    credential.chmod(0o600)
    env = {key: value for key, value in os.environ.items() if not key.startswith("OCTET_")}
    env.update({
        "HOME": str(home), "TERM": "xterm-256color", "COLORTERM": "truecolor",
        "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "OCTET_COLOR_SCHEME": "light",
    })
    return {"home": home, "workspace": workspace, "sessions": sessions,
            "env": env, "model": "custom/scripted/mock-1", "name": name}


def install_python_example(root: Path) -> Path:
    extensions = root / "extensions"
    shutil.copytree(EXAMPLES / "word-count-python", extensions / "word-count-python")
    shutil.copytree(
        REPO / "sdk" / "python" / "octet_extension",
        extensions / "word-count-python" / "vendor" / "octet_extension",
    )
    return extensions


def install_typescript_example(root: Path) -> Path:
    extensions = root / "extensions"
    extension = extensions / "word-count-typescript"
    shutil.copytree(EXAMPLES / "word-count-typescript", extension)
    for command in (
        ["npm", "pack", str(REPO / "sdk" / "typescript" / "process"),
         "--pack-destination", str(extension), "--ignore-scripts"],
        ["npm", "install", "--offline", "--ignore-scripts", "--no-package-lock"],
        [str(extension / "node_modules" / ".bin" / "octet-extension"), "manifest",
         "extension.ts", "--name", "word-count-typescript", "--version", "0.1.0",
         "--out", "extension.toml", "--filesystem", "workspace"],
    ):
        subprocess.run(command, cwd=extension, check=True, capture_output=True, text=True, timeout=120)
    return extensions


class WordCountExampleTest(unittest.TestCase):
    binary: Path

    @classmethod
    def setUpClass(cls):
        binary = find_binary()
        if binary is None:
            raise unittest.SkipTest("no octet binary; set OCTET_BIN or build bin/octet-latest")
        cls.binary = binary

    def scratch(self, language: str) -> Path:
        return Path(tempfile.mkdtemp(prefix=f"octet-word-count-{language}-"))

    def assert_tool_call(self, extensions: Path, name: str):
        provider = ScriptedProvider()
        try:
            env = make_env(self.scratch("print"), provider.port, name)
            result = subprocess.run(
                [str(self.binary), "--offline", "--no-context-files",
                 "--workspace", str(env["workspace"]), "--session-dir", str(env["sessions"]),
                 "--model", env["model"], "--extension-dir", str(extensions),
                 "--enable-extension", name, "--print",
                 "Use the word_count tool on sample.txt and report the count."],
                cwd=env["workspace"], env=env["env"], capture_output=True, text=True, timeout=180,
            )
        finally:
            provider.close()
        self.assertEqual(provider.errors, [])
        self.assertTrue(any(EXPECTED in value for value in provider.tool_results), provider.tool_results)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(EXPECTED, result.stdout)

    def assert_slash_command(self, extensions: Path, name: str):
        if OctetSession is None:
            self.skipTest("scripts/e2e PTY harness is unavailable")
        provider = ScriptedProvider()
        root = self.scratch("tui")
        try:
            env = make_env(root, provider.port, name)
            args = [str(self.binary), "--offline", "--no-context-files", "--color", "always",
                    "--workspace", str(env["workspace"]), "--session-dir", str(env["sessions"]),
                    "--model", env["model"], "--mouse", "app", "--no-tools", "--name", f"R1-{name}",
                    "--extension-dir", str(extensions), "--enable-extension", name]
            session = OctetSession(self.binary, args=args, env=env["env"],
                                   cwd=env["workspace"], log_path=root / "tui.ansi")
            try:
                session.wait_ready(env["model"], timeout=60)
                session.submit("/wordcount sample.txt")
                session.wait_for_text(EXPECTED, timeout=60)
            finally:
                session.close()
        finally:
            provider.close()

    def test_python_example_tool_call(self):
        self.assert_tool_call(install_python_example(self.scratch("python-tool")), "word-count-python")

    def test_python_example_slash_command(self):
        self.assert_slash_command(install_python_example(self.scratch("python-tui")), "word-count-python")

    @unittest.skipUnless(shutil.which("node") and shutil.which("npm"), "Node >=22.19 and npm are required")
    def test_typescript_example_tool_call(self):
        self.assert_tool_call(install_typescript_example(self.scratch("typescript-tool")), "word-count-typescript")

    @unittest.skipUnless(shutil.which("node") and shutil.which("npm"), "Node >=22.19 and npm are required")
    def test_typescript_example_slash_command(self):
        self.assert_slash_command(install_typescript_example(self.scratch("typescript-tui")), "word-count-typescript")


if __name__ == "__main__":
    unittest.main(verbosity=2)

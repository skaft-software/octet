"""Deterministic OpenAI-compatible mock provider for the E2E suite.

The suite never calls a paid model: every octet process under test points its
custom provider credential at this in-process HTTP server. Responses are
scripted per request content, can stream chunk by chunk, and can hold the turn
open at a gate until the check releases it, which makes "while busy" assertions
deterministic instead of timing-dependent.
"""

from __future__ import annotations

import json
import threading
import time
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Callable


@dataclass
class Reply:
    """One scripted assistant response."""

    text: str = ""
    reasoning: str = ""
    #: Delay between streamed chunks in seconds.
    chunk_delay: float = 0.0
    #: Split the text into this many characters per chunk (0: whole words).
    chunk_chars: int = 0
    #: Hold the turn open after the text until this event is set.
    gate: threading.Event | None = None
    gate_timeout: float = 20.0
    usage: bool = True


@dataclass
class RequestRecord:
    messages: list[dict]
    stream: bool
    received_at: float
    body: dict = field(default_factory=dict)

    def user_text(self) -> str:
        parts = []
        for message in self.messages:
            if message.get("role") != "user":
                continue
            content = message.get("content")
            if isinstance(content, str):
                parts.append(content)
            elif isinstance(content, list):
                for item in content:
                    if isinstance(item, dict) and item.get("type") == "text":
                        parts.append(str(item.get("text", "")))
        return "\n".join(parts)


class _Rule:
    def __init__(self, matcher: Callable[[RequestRecord], bool], reply: Callable[[RequestRecord], Reply]):
        self.matcher = matcher
        self.reply = reply


#: A rule that always matches is the fallback; `contains` is the common sugar.
def contains(*needles: str) -> Callable[[RequestRecord], bool]:
    def match(request: RequestRecord) -> bool:
        text = request.user_text()
        return all(needle in text for needle in needles)

    return match


class MockProvider:
    """Loopback HTTP server implementing `/v1/models` and chat completions."""

    def __init__(self, model_id: str = "mock-1"):
        self.model_id = model_id
        self.requests: list[RequestRecord] = []
        self._rules: list[_Rule] = []
        self._lock = threading.Lock()
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), self._handler_class())
        self._server.daemon_threads = True
        self.port = self._server.server_address[1]
        self._thread = threading.Thread(target=self._server.serve_forever, name="mock-provider", daemon=True)
        self._thread.start()

    # ------------------------------------------------------------- scripting
    def rule(self, matcher: Callable[[RequestRecord], bool], reply: Callable[[RequestRecord], Reply]) -> None:
        with self._lock:
            self._rules.append(_Rule(matcher, reply))

    def reply_with(self, matcher: Callable[[RequestRecord], bool], reply: Reply) -> None:
        self.rule(matcher, lambda _request: reply)

    def reply_text(self, matcher: Callable[[RequestRecord], bool], text: str, **kwargs) -> None:
        self.reply_with(matcher, Reply(text=text, **kwargs))

    def default_reply(self, text: str, **kwargs) -> None:
        self.reply_with(lambda _request: True, Reply(text=text, **kwargs))

    def request_matching(self, needle: str) -> list[RequestRecord]:
        with self._lock:
            return [record for record in self.requests if needle in record.user_text()]

    def wait_for_request(self, needle: str, timeout: float = 30.0) -> RequestRecord:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            matches = self.request_matching(needle)
            if matches:
                return matches[-1]
            time.sleep(0.02)
        raise AssertionError(f"mock provider saw no request containing {needle!r} within {timeout}s")

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()

    # ------------------------------------------------------------- internals
    def _respond(self, request: RequestRecord) -> Reply:
        with self._lock:
            rules = list(self._rules)
        for rule in rules:
            if rule.matcher(request):
                return rule.reply(request)
        return Reply(text=f"MOCK-DEFAULT {request.user_text()[:60]}")

    def _handler_class(self):
        provider = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):  # noqa: D102 - silence per-request logging
                pass

            def do_GET(self):  # noqa: N802 - BaseHTTPRequestHandler API
                body = json.dumps(
                    {
                        "object": "list",
                        "data": [{"id": provider.model_id, "object": "model"}],
                    }
                ).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):  # noqa: N802 - BaseHTTPRequestHandler API
                length = int(self.headers.get("Content-Length", "0"))
                try:
                    body = json.loads(self.rfile.read(length) or b"{}")
                except json.JSONDecodeError:
                    body = {}
                record = RequestRecord(
                    messages=body.get("messages", []),
                    stream=bool(body.get("stream")),
                    received_at=time.monotonic(),
                    body=body,
                )
                with provider._lock:
                    provider.requests.append(record)
                reply = provider._respond(record)
                if record.stream:
                    self._stream(record, reply)
                else:
                    self._complete(record, reply)

            def _chunks(self, reply: Reply):
                if reply.reasoning:
                    for piece in self._split(reply.reasoning):
                        yield {"reasoning_content": piece}
                for piece in self._split(reply.text):
                    yield {"content": piece}

            @staticmethod
            def _split(text: str) -> list[str]:
                if not text:
                    return []
                if "\n" in text:
                    parts = text.split("\n")
                    return [part + "\n" for part in parts[:-1]] + ([parts[-1]] if parts[-1] else [])
                return [part + " " for part in text.split(" ") if part]

            def _write_chunk(self, payload: dict) -> None:
                data = f"data: {json.dumps(payload)}\n\n".encode()
                self.wfile.write(b"%x\r\n" % len(data) + data + b"\r\n")
                self.wfile.flush()

            def _stream(self, record: RequestRecord, reply: Reply) -> None:
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Cache-Control", "no-cache")
                self.send_header("Connection", "close")
                self.send_header("Transfer-Encoding", "chunked")
                self.end_headers()
                base = {"id": "chatcmpl-mock", "object": "chat.completion.chunk", "model": provider.model_id}
                try:
                    self._write_chunk({**base, "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": None}]})
                    for delta in self._chunks(reply):
                        if reply.chunk_delay:
                            time.sleep(reply.chunk_delay)
                        self._write_chunk(
                            {**base, "choices": [{"index": 0, "delta": delta, "finish_reason": None}]}
                        )
                    if reply.gate is not None:
                        reply.gate.wait(timeout=reply.gate_timeout)
                    final: dict = {
                        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                    }
                    if reply.usage:
                        words = max(1, len((reply.text or " ").split()))
                        final["usage"] = {
                            "prompt_tokens": 16,
                            "completion_tokens": words,
                            "total_tokens": 16 + words,
                        }
                    self._write_chunk({**base, **final})
                    done = b"data: [DONE]\n\n"
                    self.wfile.write(b"%x\r\n" % len(done) + done + b"\r\n")
                    self.wfile.write(b"0\r\n\r\n")
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    # The client cancelled the turn (shutdown, abort). Nothing
                    # for the mock to do; the check asserts on what octet shows.
                    pass

            def _complete(self, record: RequestRecord, reply: Reply) -> None:
                payload = {
                    "id": "chatcmpl-mock",
                    "object": "chat.completion",
                    "model": provider.model_id,
                    "choices": [
                        {
                            "index": 0,
                            "message": {"role": "assistant", "content": reply.text},
                            "finish_reason": "stop",
                        }
                    ],
                    "usage": {"prompt_tokens": 16, "completion_tokens": 8, "total_tokens": 24},
                }
                body = json.dumps(payload).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        return Handler


def numbered_lines(prefix: str, count: int, words: str = "terminal regression evidence") -> str:
    """The U8 long-response shape: numbered lines with distinct sentences."""
    lines = []
    for index in range(1, count + 1):
        lines.append(f"{index:03d}. {prefix}-{index:03d} {words} {index}")
    return "\n".join(lines)


def list_lines(prefix: str, count: int, words: str = "cell") -> str:
    """Distinct lines that stay byte-identical when the TUI renders markdown.

    Plain consecutive lines merge into one wrapped paragraph, and a list that
    starts at `009.` is renumbered from 9; an ordered list starting at 1 with
    consecutive numbers renders exactly as written, so cell-exact selection
    assertions use this shape.
    """
    return "\n".join(f"{index}. {prefix}-{index:03d} {words} {index}" for index in range(1, count + 1))

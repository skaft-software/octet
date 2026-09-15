"""Real Pi 0.84.4 aggregate journey used by the integrity-checked full gate.

This module is deliberately inert until conformance.py is given the verifier's
local package, tarballs, and clean pinned source checkout.  It never installs
packages or rewrites the selected source.  The caller supplies the already
calculated identity values so this protocol runner cannot accidentally weaken
the existing source/package checks.
"""
from __future__ import annotations

import json
import os
import selectors
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any, Callable, Sequence


MAX_FRAME_BYTES = 1 * 1024 * 1024
MAX_MESSAGES = 512
MAX_STDERR_BYTES = 64 * 1024
READ_CHUNK_BYTES = 64 * 1024


class RealRuntimeFailure(RuntimeError):
    """A bounded failure from the real-runtime protocol journey."""


class JsonRpcPeer:
    """Bounded byte-framed JSON-RPC peer for the selected real-runtime child."""

    def __init__(self, command: Sequence[str], cwd: Path, env: dict[str, str]) -> None:
        try:
            self.process = subprocess.Popen(
                list(command),
                cwd=cwd,
                env=env,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                bufsize=0,
            )
        except OSError as error:
            raise RealRuntimeFailure("could not launch the pinned Pi aggregate") from error
        self.messages: list[dict[str, Any]] = []
        self.stderr = bytearray()
        self._next_id = 1
        self._stdout_buffer = bytearray()
        self._pending: list[dict[str, Any]] = []
        self._message_count = 0
        self._selector = selectors.DefaultSelector()
        self._stdout_open = False
        self._stderr_open = False
        try:
            if self.process.stdout is None or self.process.stderr is None:
                raise RealRuntimeFailure("pinned Pi aggregate output pipes are unavailable")
            for stream, kind in ((self.process.stdout, "stdout"), (self.process.stderr, "stderr")):
                os.set_blocking(stream.fileno(), False)
                self._selector.register(stream, selectors.EVENT_READ, kind)
            self._stdout_open = True
            self._stderr_open = True
        except (OSError, ValueError) as error:
            self.close()
            raise RealRuntimeFailure("could not configure bounded Pi aggregate output") from error

    def send(self, message: dict[str, Any]) -> None:
        if self.process.stdin is None:
            raise RealRuntimeFailure("pinned Pi aggregate stdin is unavailable")
        try:
            frame = json.dumps(message, separators=(",", ":")).encode("utf-8") + b"\n"
        except (TypeError, ValueError) as error:
            raise RealRuntimeFailure("pinned Pi aggregate request is not JSON serializable") from error
        if len(frame) > MAX_FRAME_BYTES:
            raise RealRuntimeFailure("pinned Pi aggregate request exceeds the protocol frame limit")
        try:
            self.process.stdin.write(frame)
            self.process.stdin.flush()
        except OSError as error:
            raise RealRuntimeFailure("pinned Pi aggregate stopped accepting protocol input") from error

    def send_request(self, method: str, params: dict[str, Any] | None = None) -> int:
        request_id = self._next_id
        self._next_id += 1
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params or {}})
        return request_id

    def _fail(self, message: str) -> RealRuntimeFailure:
        detail = bytes(self.stderr).decode("utf-8", "replace")[-1000:]
        if detail:
            message += f"; stderr={detail!r}"
        return RealRuntimeFailure(message)

    def _unregister(self, stream: Any) -> None:
        try:
            self._selector.unregister(stream)
        except (KeyError, ValueError):
            pass

    def _parse_stdout(self) -> None:
        while True:
            try:
                end = self._stdout_buffer.index(b"\n")
            except ValueError:
                if len(self._stdout_buffer) > MAX_FRAME_BYTES:
                    raise self._fail("pinned Pi aggregate protocol frame exceeds the size limit")
                return
            frame = bytes(self._stdout_buffer[:end])
            del self._stdout_buffer[: end + 1]
            if len(frame) > MAX_FRAME_BYTES:
                raise self._fail("pinned Pi aggregate protocol frame exceeds the size limit")
            try:
                message = json.loads(frame.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                raise self._fail("pinned Pi aggregate wrote non-JSON protocol output") from error
            if not isinstance(message, dict):
                raise self._fail("pinned Pi aggregate wrote a non-object protocol message")
            self._message_count += 1
            if self._message_count > MAX_MESSAGES:
                raise self._fail("pinned Pi aggregate exceeded the protocol message limit")
            self.messages.append(message)
            self._pending.append(message)

    def _read_message(self, deadline: float, description: str) -> dict[str, Any]:
        if self._pending:
            return self._pending.pop(0)
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise self._fail(f"pinned Pi aggregate timed out waiting for {description}")
            try:
                ready = self._selector.select(remaining)
            except OSError as error:
                raise self._fail("could not read pinned Pi aggregate output") from error
            if not ready:
                raise self._fail(f"pinned Pi aggregate timed out waiting for {description}")
            for key, _ in ready:
                stream = key.fileobj
                kind = key.data
                try:
                    chunk = os.read(stream.fileno(), READ_CHUNK_BYTES)
                except BlockingIOError:
                    continue
                except OSError as error:
                    raise self._fail("could not read pinned Pi aggregate output") from error
                if not chunk:
                    self._unregister(stream)
                    if kind == "stdout":
                        self._stdout_open = False
                        if self._stdout_buffer:
                            raise self._fail("pinned Pi aggregate ended with an incomplete protocol frame")
                        raise self._fail("pinned Pi aggregate exited before completing the real journey")
                    self._stderr_open = False
                    continue
                if kind == "stderr":
                    if len(self.stderr) + len(chunk) > MAX_STDERR_BYTES:
                        raise self._fail("pinned Pi aggregate stderr exceeds the size limit")
                    self.stderr.extend(chunk)
                    continue
                self._stdout_buffer.extend(chunk)
                self._parse_stdout()
            if self._pending:
                return self._pending.pop(0)

    def wait_for(
        self,
        predicate: Callable[[dict[str, Any]], bool],
        *,
        description: str,
        timeout: float = 20.0,
    ) -> dict[str, Any]:
        for message in self.messages:
            if predicate(message):
                return message
        deadline = time.monotonic() + timeout
        while True:
            message = self._read_message(deadline, description)
            if predicate(message):
                return message

    def response(self, request_id: int, *, timeout: float = 20.0) -> dict[str, Any]:
        def is_response(message: dict[str, Any]) -> bool:
            return message.get("id") == request_id and "method" not in message

        response = self.wait_for(is_response, description=f"response {request_id}", timeout=timeout)
        if "error" in response:
            return response
        if "result" not in response:
            raise RealRuntimeFailure("pinned Pi aggregate returned an invalid JSON-RPC response")
        return response

    def close(self) -> None:
        if getattr(self.process, "stdin", None) is not None:
            try:
                self.process.stdin.close()
            except OSError:
                pass
            self.process.stdin = None
        try:
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=3)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise RealRuntimeFailure("pinned Pi aggregate did not cleanly terminate") from error
        for stream in (self.process.stdout, self.process.stderr):
            if stream is not None:
                self._unregister(stream)
                try:
                    stream.close()
                except OSError:
                    pass
        try:
            self._selector.close()
        except OSError:
            pass
        if self.process.poll() is None:
            raise RealRuntimeFailure("pinned Pi aggregate remained alive after cleanup")


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise RealRuntimeFailure(message)


def _notification(peer: JsonRpcPeer, exact: str | None = None, prefix: str | None = None) -> dict[str, Any]:
    def matches(message: dict[str, Any]) -> bool:
        if message.get("method") != "notification":
            return False
        value = str(message.get("params", {}).get("message", ""))
        return (exact is None or value == exact) and (prefix is None or value.startswith(prefix))

    return peer.wait_for(matches, description="Pi notification")


def _strict_command(
    *,
    launcher: Sequence[str] | None = None,
    command_builder: Callable[[Sequence[str]], Sequence[str]] | None = None,
    bridge: Path,
    package: Path,
    extensions: Sequence[Path],
    source_fingerprints: Sequence[str],
    source_lock_fingerprints: Sequence[str],
    runtime_integrity: str,
    aggregate_digest: str,
    manifest_path: Path,
    link_identity: str,
    agent_dir: Path,
    octet_version: str,
    command_name: str,
) -> list[str]:
    child = [str(bridge), "--pi-package", str(package)]
    for extension in extensions:
        child.extend(["--extension", str(extension)])
    for digest in source_fingerprints:
        child.extend(["--source-fingerprint", digest])
    for digest in source_lock_fingerprints:
        child.extend(["--source-lock-fingerprint", digest])
    child.extend(
        [
            "--agent-dir",
            str(agent_dir),
            "--pi-runtime-integrity",
            runtime_integrity,
            "--aggregate-digest",
            aggregate_digest,
            "--link-manifest",
            str(manifest_path),
            "--link-identity",
            link_identity,
            "--octet-version",
            octet_version,
            "--command",
            command_name,
        ]
    )
    if command_builder is not None:
        return list(command_builder(child))
    _require(launcher is not None, "real aggregate launcher is unavailable")
    return [*launcher, *child]


def _initialize_params(checkout: Path, command_name: str, manifest_path: Path, octet_version: str) -> dict[str, Any]:
    return {
        "workspace": str(checkout),
        "host": {},
        "protocol": {"optional_features": ["runtime_commands"]},
        "octet_version": octet_version,
        "extension": {
            "name": command_name,
            "version": "real-runtime-fixture",
            "manifest_path": str(manifest_path),
            "source": "explicit",
        },
    }


def _run_once(
    *,
    command: Sequence[str],
    checkout: Path,
    env: dict[str, str],
    command_name: str,
    manifest_path: Path,
    octet_version: str,
    expected: dict[str, Any],
) -> dict[str, Any]:
    peer = JsonRpcPeer(command, checkout, env)
    try:
        initialize_id = peer.send_request(
            "initialize", _initialize_params(checkout, command_name, manifest_path, octet_version)
        )
        initialized = peer.response(initialize_id)
        _require("error" not in initialized, "pinned Pi aggregate initialization failed")
        result = initialized["result"]
        tools = result.get("tools", [])
        commands = result.get("commands", [])
        tool_names = [item.get("name") for item in tools if isinstance(item, dict)]
        command_names = [item.get("name") for item in commands if isinstance(item, dict)]
        for name in expected["tool_names"]:
            _require(name in tool_names, f"real aggregate did not register expected tool {name}")
        prefix = expected["command_order_prefix"]
        _require(
            command_names[: len(prefix)] == prefix,
            "real aggregate command registration order differs from the pinned fixture",
        )

        session_id = peer.send_request("session/started", {})
        session_response = peer.response(session_id)
        _require("error" not in session_response, "real aggregate session-start lifecycle failed")
        _notification(peer, exact=expected["session_notification"])

        hello_id = peer.send_request(
            "tool/call",
            {"name": "hello", "arguments": {"name": "aggregate"}, "catalog_revision": 0},
        )
        hello_response = peer.response(hello_id)
        _require("error" not in hello_response, "real aggregate hello tool call failed")
        hello_content = hello_response["result"].get("content", [])
        _require(
            hello_content and hello_content[0].get("text") == expected["hello_text"],
            "real aggregate hello result differs from the unchanged Pi example",
        )

        emit_id = peer.send_request(
            "command/execute", {"name": "emit", "arguments": ["aggregate event"]}
        )
        emit_response = peer.response(emit_id)
        _require("error" not in emit_response, "real aggregate event-bus command failed")
        _notification(peer, prefix=expected["emit_notification_prefix"])

        cancel_id = peer.send_request("command/execute", {"name": "timed", "arguments": []})
        host_request = peer.wait_for(
            lambda message: message.get("method") == "confirmation/request" and "id" in message,
            description="real Pi confirmation request",
        )
        peer.send({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": cancel_id}})
        cancelled = peer.response(cancel_id)
        _require(
            cancelled.get("error", {}).get("code") == expected["cancelled_error_code"],
            "real aggregate cancellation did not return the bridge cancellation code",
        )
        peer.wait_for(
            lambda message: message.get("method") == "$/cancelRequest"
            and message.get("params", {}).get("id") == host_request.get("id"),
            description="cancelled host confirmation",
        )

        settled_id = peer.send_request("session/settled", {"outcome": "cancelled"})
        settled_response = peer.response(settled_id)
        _require("error" not in settled_response, "real aggregate session settlement failed")
        shutdown_id = peer.send_request("shutdown", {})
        shutdown_response = peer.response(shutdown_id)
        _require("error" not in shutdown_response, "real aggregate shutdown failed")
        return {
            "catalog": {"tools": tool_names, "commands": command_names},
            "hello": hello_content[0].get("text"),
            "session_notification": expected["session_notification"],
            "event_bus": True,
            "cancellation": "cancelled",
        }
    finally:
        peer.close()


def _rejection(
    *,
    command: Sequence[str],
    checkout: Path,
    env: dict[str, str],
    command_name: str,
    manifest_path: Path,
    wrong_manifest_path: Path,
    octet_version: str,
    expected_fragment: str,
) -> None:
    peer = JsonRpcPeer(command, checkout, env)
    try:
        initialize_id = peer.send_request(
            "initialize", _initialize_params(checkout, command_name, wrong_manifest_path, octet_version)
        )
        response = peer.response(initialize_id)
        message = str(response.get("error", {}).get("message", ""))
        _require(expected_fragment in message, "strict trust identity did not reject the mismatched manifest")
        _require(str(wrong_manifest_path) not in message, "strict trust rejection exposed the selected manifest path")
        _require(str(manifest_path) not in message, "strict trust rejection exposed the trusted manifest path")
    finally:
        peer.close()


def run_real_aggregate(
    *,
    launcher: Sequence[str] | None = None,
    command_builder: Callable[[Sequence[str]], Sequence[str]] | None = None,
    bridge: Path,
    checkout: Path,
    package: Path,
    extensions: Sequence[Path],
    source_fingerprints: Sequence[str],
    source_lock_fingerprints: Sequence[str],
    runtime_integrity: str,
    aggregate_digest: str,
    manifest_path: Path,
    alternate_manifest_path: Path,
    link_identity: str,
    make_link_identity: Callable[..., str],
    agent_dir: Path,
    octet_version: str,
    command_name: str,
    env: dict[str, str],
    expected: dict[str, Any],
) -> dict[str, Any]:
    """Run the ordered real aggregate, restart, stale-source, and trust gates."""
    _require(len(extensions) == len(source_fingerprints), "real aggregate source identity is incomplete")
    _require(len(extensions) == len(source_lock_fingerprints), "real aggregate lock identity is incomplete")
    command = _strict_command(
        launcher=launcher,
        command_builder=command_builder,
        bridge=bridge,
        package=package,
        extensions=extensions,
        source_fingerprints=source_fingerprints,
        source_lock_fingerprints=source_lock_fingerprints,
        runtime_integrity=runtime_integrity,
        aggregate_digest=aggregate_digest,
        manifest_path=manifest_path,
        link_identity=link_identity,
        agent_dir=agent_dir,
        octet_version=octet_version,
        command_name=command_name,
    )
    first = _run_once(
        command=command,
        checkout=checkout,
        env=env,
        command_name=command_name,
        manifest_path=manifest_path,
        octet_version=octet_version,
        expected=expected,
    )
    second = _run_once(
        command=command,
        checkout=checkout,
        env=env,
        command_name=command_name,
        manifest_path=manifest_path,
        octet_version=octet_version,
        expected=expected,
    )
    _require(first == second, "real aggregate restart did not reproduce the first catalog and observations")

    _rejection(
        command=command,
        checkout=checkout,
        env=env,
        command_name=command_name,
        manifest_path=manifest_path,
        wrong_manifest_path=alternate_manifest_path,
        octet_version=octet_version,
        expected_fragment=expected["trust_error_fragment"],
    )

    stale_source = extensions[0]
    with tempfile.TemporaryDirectory(prefix="octet-pi-stale-source-", dir=agent_dir.parent) as directory:
        copied = Path(directory) / stale_source.name
        shutil.copyfile(stale_source, copied)
        copied.write_bytes(copied.read_bytes() + b"\n// verifier-only stale copy mutation\n")
        stale_extensions = [copied, *extensions[1:]]
        stale_hashes = list(source_fingerprints)
        stale_locks = list(source_lock_fingerprints)
        stale_identity = make_link_identity(
            extensions=stale_extensions,
            source_fingerprints=stale_hashes,
            source_lock_fingerprints=stale_locks,
            manifest_path=manifest_path,
        )
        stale_command = _strict_command(
            launcher=launcher,
            command_builder=command_builder,
            bridge=bridge,
            package=package,
            extensions=stale_extensions,
            source_fingerprints=stale_hashes,
            source_lock_fingerprints=stale_locks,
            runtime_integrity=runtime_integrity,
            aggregate_digest=aggregate_digest,
            manifest_path=manifest_path,
            link_identity=stale_identity,
            agent_dir=agent_dir,
            octet_version=octet_version,
            command_name=command_name,
        )
        peer = JsonRpcPeer(stale_command, checkout, env)
        try:
            initialize_id = peer.send_request(
                "initialize", _initialize_params(checkout, command_name, manifest_path, octet_version)
            )
            response = peer.response(initialize_id)
            message = str(response.get("error", {}).get("message", ""))
            _require(
                expected["stale_source_error_fragment"] in message,
                "real aggregate stale-source identity was not rejected",
            )
            _require(str(directory) not in message, "real aggregate stale-source rejection exposed a temporary path")
        finally:
            peer.close()

    return {
        "status": "integrity_verified_local_full_run",
        "restart": "passed",
        "ordered_registration": "passed",
        "event_bus": "passed",
        "cancellation": "passed",
        "stale_source_rejection": "passed",
        "trust_binding": "passed",
        "globalThis": "unrun_without_an_unchanged_marker",
        "observation": first,
    }

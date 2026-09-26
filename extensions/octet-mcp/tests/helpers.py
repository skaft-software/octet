"""Test helpers for the package-owned octet-mcp suite."""

from __future__ import annotations

from dataclasses import replace
from pathlib import Path
import sys
import time
from typing import Any, Mapping, Optional


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "fixtures"


class FakeCancellation:
    def __init__(self) -> None:
        self.cancelled = False
        self.reason: Optional[str] = None

    def cancel(self, reason: str = "test") -> None:
        self.reason = reason
        self.cancelled = True

    def raise_if_cancelled(self) -> None:
        if self.cancelled:
            from octet_extension import CancelledError

            raise CancelledError(self.reason or "test")


class FakeExtension:
    def __init__(
        self, scratch: Path, *, policy: str = "deny", confirm: Optional[bool] = None
    ) -> None:
        self.negotiated_features = frozenset(
            {"dynamic_tools", "content_parts", "artifacts", "request_progress", "policy_intents"}
        )
        self.cancellation = FakeCancellation()
        self.request_id = 77
        self._revision = 0
        self._tools: dict[str, dict[str, Any]] = {}
        self.catalogs: dict[int, dict[str, dict[str, Any]]] = {0: {}}
        self.progress_events: list[dict[str, Any]] = []
        self.artifacts: dict[str, tuple[str, bytes]] = {}
        self.presentations: list[dict[str, Any]] = []
        self.policy = policy
        self.scratch = scratch
        # `None` means the confirmation surface is unavailable.
        self.confirm_answer = confirm
        self.confirm_calls: list[dict[str, Any]] = []

    def register_tools(self, definitions: list[Mapping[str, Any]]) -> dict[str, Any]:
        for definition in definitions:
            self._tools[str(definition["name"])] = dict(definition)
        self._revision += 1
        self.catalogs[self._revision] = dict(self._tools)
        return {"revision": self._revision, "tools": sorted(self._tools)}

    def unregister_tools(self, *names: str) -> dict[str, Any]:
        for name in names:
            self._tools.pop(name, None)
        self._revision += 1
        self.catalogs[self._revision] = dict(self._tools)
        return {"revision": self._revision, "tools": sorted(self._tools)}

    def publish_artifact(
        self, *, mime_type: str, path: str, size: int, sha256: str
    ) -> str:
        del sha256
        data = (self.scratch / path).read_bytes()
        assert len(data) == size
        artifact_id = f"artifact-{len(self.artifacts) + 1}"
        self.artifacts[artifact_id] = (mime_type, data)
        return artifact_id

    def progress(self, event: Any = None, **kwargs: Any) -> None:
        self.progress_events.append({"event": event, **kwargs})

    def evaluate_policy(
        self, intent: Mapping[str, Any], *, approval_token: Optional[str] = None
    ) -> dict[str, Any]:
        del intent, approval_token
        return {"decision": self.policy}

    def confirm(self, prompt: str, **kwargs: Any) -> bool:
        self.confirm_calls.append({"prompt": prompt, **kwargs})
        if self.confirm_answer is None:
            raise RuntimeError("no interactive confirmation surface")
        return self.confirm_answer

    def publish_presentation(self, snapshot: Mapping[str, Any]) -> None:
        value = dict(snapshot)
        if self.presentations:
            assert value["revision"] > self.presentations[-1]["revision"]
        self.presentations.append(value)


def limits(**overrides: Any):
    from octet_mcp.config import Limits

    return replace(Limits(), **overrides)


def server_config(
    scenario: str = "stable",
    *,
    server_id: str = "fixture",
    extra_args: tuple[str, ...] = (),
    environment: Optional[dict[str, str]] = None,
    inherited_environment: tuple[str, ...] = (),
    confirm_unknown_tools: bool = False,
    request_timeout_ms: int = 500,
    startup_timeout_ms: int = 1000,
    max_restarts: int = 1,
):
    from octet_mcp.config import ServerConfig

    return ServerConfig(
        id=server_id,
        label="Fixture server",
        command=sys.executable,
        args=(
            str(FIXTURES / "fake_mcp_server.py"),
            "--scenario",
            scenario,
            *extra_args,
        ),
        cwd=ROOT,
        environment=environment or {},
        inherited_environment=inherited_environment,
        confirm_unknown_tools=confirm_unknown_tools,
        enabled=True,
        required=False,
        startup_timeout_ms=startup_timeout_ms,
        request_timeout_ms=request_timeout_ms,
        max_restarts=max_restarts,
        scope="user",
    )


def real_server_config(*, confirm_unknown_tools: bool = False):
    from octet_mcp.config import ServerConfig

    return ServerConfig(
        id="real-fixture",
        label="Real local fixture",
        command=sys.executable,
        args=(str(FIXTURES / "real_mcp_server.py"),),
        cwd=ROOT,
        environment={},
        confirm_unknown_tools=confirm_unknown_tools,
        enabled=True,
        required=True,
        startup_timeout_ms=2000,
        request_timeout_ms=2000,
        max_restarts=1,
        scope="user",
    )


def wait_for(predicate, timeout: float = 4.0, message: str = "condition") -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.01)
    raise AssertionError(f"timed out waiting for {message}")

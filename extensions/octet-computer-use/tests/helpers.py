"""Test doubles for the octet computer-use suite."""

from __future__ import annotations

from typing import Any, Dict, List, Mapping, Optional


OWNER_CONTEXT = {
    "resource_owner": {
        "session_id": "session-owner",
        "extension_instance_id": "instance-owner",
        "process_generation": 1,
    },
    "workspace": "/tmp/workspace",
    "host": {},
}


class RecordingExtension:
    """A stand-in for the SDK Extension that records confirmation requests.

    ``confirm=None`` models a frontend with no interactive confirmation surface,
    which must fail closed rather than assume approval.
    """

    def __init__(self, confirm: Optional[bool] = True) -> None:
        self.confirmations: List[Dict[str, Any]] = []
        self.confirm_answer = confirm

    def confirm(self, prompt: str, **kwargs: Any) -> bool:
        self.confirmations.append({"prompt": prompt, **kwargs})
        if self.confirm_answer is None:
            raise RuntimeError("no interactive confirmation surface")
        return bool(self.confirm_answer)


class FakeClient:
    """A driver client double with an explicit read-only/effectful split."""

    def __init__(
        self,
        *,
        read_only: Optional[List[str]] = None,
        effectful: Optional[List[str]] = None,
        result: Optional[Mapping[str, Any]] = None,
        raises: Optional[Exception] = None,
    ) -> None:
        from octet_computer_use.driver_client import ToolInfo

        self._catalog = {}
        for name in read_only or []:
            self._catalog[name] = ToolInfo(name, f"{name} description", {}, True)
        for name in effectful or []:
            self._catalog[name] = ToolInfo(name, f"{name} description", {}, False)
        self._result = dict(result or {"content": [{"type": "text", "text": "ok"}]})
        self._raises = raises
        self.calls: List[Any] = []
        self.started = True

    def tools(self) -> List[Any]:
        return list(self._catalog.values())

    def requires_confirmation(self, tool: str) -> bool:
        info = self._catalog.get(tool)
        return info is None or not info.read_only

    def call(self, tool: str, arguments: Any = None, **kwargs: Any) -> Dict[str, Any]:
        self.calls.append((tool, dict(arguments or {})))
        if self._raises is not None:
            raise self._raises
        return dict(self._result)

    def close(self) -> None:
        self.started = False

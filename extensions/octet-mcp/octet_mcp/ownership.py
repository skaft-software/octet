"""Immutable host-issued owner fence for remote MCP state (never tool arguments)."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Mapping, Optional


@dataclass(frozen=True, repr=False)
class ResourceOwner:
    session_id: str
    extension_instance_id: str
    process_generation: int

    @classmethod
    def from_context(cls, context: Mapping[str, Any]) -> Optional[ResourceOwner]:
        value = context.get("resource_owner")
        if not isinstance(value, Mapping) or set(value) != {"session_id", "extension_instance_id", "process_generation"}:
            return None
        session = value["session_id"]
        instance = value["extension_instance_id"]
        generation = value["process_generation"]
        if (not all(isinstance(item, str) and 0 < len(item) <= 256 and all(33 <= ord(c) <= 126 for c in item) for item in (session, instance))
                or type(generation) is not int or not 1 <= generation <= 2**53 - 1):
            return None
        return cls(session, instance, generation)

    def wire(self) -> dict[str, Any]:
        return {"session_id": self.session_id, "extension_instance_id": self.extension_instance_id, "process_generation": self.process_generation}

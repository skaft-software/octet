"""Actual SDK fixture with a fault-only stdout adapter, never a fake RPC peer."""
from dataclasses import dataclass, field
import json
import os
import sys
from typing import Optional

from typed_fixture import ext, record


@dataclass
class Defaults:
    required_nullable: Optional[str]
    default_nullable: Optional[str] = "fallback"
    count: int = 7
    values: list[int] = field(default_factory=list)


@ext.typed_tool(name="defaults", description="Distinguish omission and null", summary=lambda value: "Defaults decoded.")
def defaults(value: Defaults) -> Defaults:
    record("defaults")
    # A mutation makes accidental shared default containers observable next call.
    value.values.append(value.count)
    return value


@dataclass
class Fault:
    mode: str


class FaultWriter:
    """Corrupt only the next SDK-generated terminal after a typed handler arms it.

    Initialization, correlation, scheduling, normal serialization and shutdown
    still run through Extension and JsonRpcTransport. Tests fault the real byte
    boundary after serialization instead of implementing another extension peer.
    """
    def __init__(self, stream):
        self.stream = stream
        self.mode = None

    def write(self, text):
        mode, self.mode = self.mode, None
        if mode is not None:
            message = json.loads(text)
            assert "result" in message and "id" in message
            record("fault_" + mode)
        if mode == "invalid":
            return self.stream.write("{\n")
        if mode == "oversize":
            return self.stream.write('"' + "x" * (1024 * 1024 + 256) + '"\n')
        if mode == "eof":
            self.stream.flush()
            os.close(self.stream.fileno())
            os._exit(0)
        if mode == "duplicate":
            self.stream.write(text)
        return self.stream.write(text)

    def flush(self):
        self.stream.flush()


writer = FaultWriter(sys.stdout)


@ext.typed_tool(name="hostile", description="Fault the next SDK terminal frame", summary=lambda value: "Fault armed.")
def hostile(value: Fault) -> int:
    if value.mode not in {"invalid", "oversize", "eof", "duplicate"}:
        raise ValueError("unknown fixture fault")
    record("hostile")
    writer.mode = value.mode
    return 42


if __name__ == "__main__":
    record("started")
    ext.run(stdout=writer)

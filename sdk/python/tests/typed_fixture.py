"""Real executable fixture for SDK and production ExtensionProcess checks."""
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import sys
from typing import Optional

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from octet_extension import (
    CancelledError, Diagnostic, DiagnosticEdit, DiagnosticFix, DiagnosticLocation,
    DiagnosticSpan, Extension, TypedResult, WorkspaceSource, tool_result, text_content,
)

LOG = Path(sys.argv[1])


def record(event):
    with LOG.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps({"pid": os.getpid(), "event": event}) + "\n")
        stream.flush()


@dataclass
class Record:
    name: str
    enabled: bool
    samples: list[float]
    note: Optional[str] = None


ext = Extension(api_version="0.4", max_concurrent_requests=1,
                supported_features=["request_cancellation", "content_parts", "request_progress"])


@ext.typed_tool(name="typed_roundtrip", description="Roundtrip a typed record", summary=lambda value: "Echoed typed record.")
def typed_roundtrip(value: Record) -> Record:
    record("typed_roundtrip")
    return value


@ext.typed_tool(name="invalid_output", description="Reject deliberately invalid typed output", summary=lambda value: "Invalid.")
def invalid_output(value: Record) -> Record:
    record("invalid_output")
    return Record(1, True, [])


@ext.typed_tool(name="typed_wait", description="Cooperatively wait until cancelled", summary=lambda value: "Finished waiting.")
def typed_wait(value: Record) -> Record:
    record("entered")
    try:
        while True:
            ext.cancellation.wait(0.01)
            ext.cancellation.raise_if_cancelled()
    except CancelledError:
        record("cancelled")
        raise


@ext.typed_tool(name="typed_progress", description="Emit ephemeral negotiated status", summary=lambda value: "Progress finished.")
def typed_progress(value: Record) -> Record:
    record("typed_progress")
    for step in (1, 2):
        ext.cancellation.raise_if_cancelled()
        ext.progress(message=f"Step {step}", current=step, total=2, unit="steps")
    return value


@ext.typed_tool(name="typed_diagnostics", description="Return a structured domain diagnostic")
def typed_diagnostics(value: Record) -> TypedResult[Record]:
    record("typed_diagnostics")
    path = LOG.parent / "input.txt"
    path.write_text("a", encoding="utf-8")
    location = DiagnosticLocation(WorkspaceSource("input.txt", hashlib.sha256(path.read_bytes()).hexdigest()),
                                  DiagnosticSpan(0, 1))
    diagnostic = Diagnostic("error", "fixture.invalid", "Fixture domain failure.", primary=location,
                            fixes=(DiagnosticFix("Replace a", (DiagnosticEdit(location, "b"),)),))
    return TypedResult(None, "Domain validation failed.", diagnostics=(diagnostic,), is_error=True)


@ext.tool(name="malformed_diagnostics", description="Refuse malformed reserved metadata")
def malformed_diagnostics(value):
    record("malformed_diagnostics")
    return tool_result(text_content("Not admitted."), metadata={"octet_diagnostics_v1": [
        {"severity": "fatal", "code": "fixture.invalid", "message": "Bad severity."}]})


@ext.on_shutdown
def shutdown(params):
    record("shutdown")


if __name__ == "__main__":
    record("started")
    ext.run()

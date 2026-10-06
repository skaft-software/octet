"""Closed, bounded Diagnostic v1 metadata; references never grant authority."""
from __future__ import annotations

import copy
from dataclasses import dataclass, fields, is_dataclass
import re
from typing import Literal, Optional, Union

from .typed import _bounded_json, PORTABLE_INTEGER

DIAGNOSTICS_KEY = "octet_diagnostics_v1"


@dataclass(frozen=True)
class WorkspaceSource:
    path: str
    revision: str
    kind: Literal["workspace"] = "workspace"


@dataclass(frozen=True)
class BlobSource:
    id: str
    kind: Literal["blob"] = "blob"


@dataclass(frozen=True)
class ArtifactSource:
    id: str
    kind: Literal["artifact"] = "artifact"


@dataclass(frozen=True)
class DiagnosticSpan:
    start_byte: int
    end_byte: int


@dataclass(frozen=True)
class DiagnosticLocation:
    source: Union[WorkspaceSource, BlobSource, ArtifactSource]
    span: DiagnosticSpan


@dataclass(frozen=True)
class DiagnosticRelated:
    message: str
    location: DiagnosticLocation


@dataclass(frozen=True)
class DiagnosticEdit:
    location: DiagnosticLocation
    replacement: str


@dataclass(frozen=True)
class DiagnosticFix:
    title: str
    edits: tuple[DiagnosticEdit, ...]


@dataclass(frozen=True)
class DiagnosticAttachment:
    kind: Literal["blob", "artifact"]
    id: str
    label: Optional[str] = None


@dataclass(frozen=True)
class Diagnostic:
    severity: Literal["error", "warning", "info", "hint"]
    code: str
    message: str
    primary: Optional[DiagnosticLocation] = None
    related: tuple[DiagnosticRelated, ...] = ()
    fixes: tuple[DiagnosticFix, ...] = ()
    attachments: tuple[DiagnosticAttachment, ...] = ()

    def to_wire(self) -> dict:
        """Validate before returning metadata; absent locations remain absent."""
        value = _wire(self)
        validate_diagnostics([value])
        return value


def _wire(value, depth=0):
    if depth > 12:
        raise ValueError("diagnostic nesting exceeds bound")
    if is_dataclass(value) and not isinstance(value, type):
        return {f.name: _wire(getattr(value, f.name), depth + 1) for f in fields(value)
                if getattr(value, f.name) is not None}
    if isinstance(value, (tuple, list)):
        if len(value) > 32:
            raise ValueError("diagnostic list exceeds bound")
        return [_wire(item, depth + 1) for item in value]
    return value


def _object(value, required, optional=()):
    if type(value) is not dict or set(value) - set(required) - set(optional) or set(required) - set(value):
        raise ValueError("invalid closed diagnostic record")


def _text(value, limit=4096, *, empty=False, controls=False):
    if type(value) is not str or not empty and not value or len(value.encode("utf-8")) > limit:
        raise ValueError("diagnostic text exceeds bound or has invalid type")
    if any(127 <= ord(c) <= 159 or ord(c) < 32 and (controls or c not in "\n\t") for c in value):
        raise ValueError("diagnostic text contains controls")


def _id(value):
    if type(value) is not str or not re.fullmatch(r"[!-~]{1,128}", value):
        raise ValueError("invalid diagnostic opaque identifier")


def _list(value, maximum):
    if type(value) is not list or len(value) > maximum:
        raise ValueError("diagnostic list exceeds bound or has invalid type")
    return value


def _location(value):
    _object(value, ("source", "span"))
    source, span = value["source"], value["span"]
    if type(source) is not dict:
        raise ValueError("invalid diagnostic source")
    if source.get("kind") == "workspace":
        _object(source, ("kind", "path", "revision"))
        path = source["path"]
        _text(path, controls=True)
        if "\\" in path or ":" in path or any(part in ("", ".", "..") for part in path.split("/")):
            raise ValueError("diagnostic workspace path must be normalized and relative")
        if type(source["revision"]) is not str or not re.fullmatch("[0-9a-f]{64}", source["revision"]):
            raise ValueError("diagnostic workspace revision must be SHA-256")
    elif source.get("kind") in ("blob", "artifact"):
        _object(source, ("kind", "id"))
        _id(source["id"])
    else:
        raise ValueError("unknown diagnostic source kind")
    _object(span, ("start_byte", "end_byte"))
    if any(type(n) is not int or not 0 <= n <= PORTABLE_INTEGER for n in span.values()) or span["start_byte"] > span["end_byte"]:
        raise ValueError("diagnostic spans must be ordered portable byte offsets")


def validate_diagnostics(values: list) -> None:
    """Validate the complete reserved wire profile (not artifact/source access)."""
    _list(values, 32)
    _bounded_json(values, max_bytes=64 * 1024)
    for value in values:
        _object(value, ("severity", "code", "message"), ("primary", "related", "fixes", "attachments"))
        if value["severity"] not in ("error", "warning", "info", "hint"):
            raise ValueError("unknown diagnostic severity")
        if type(value["code"]) is not str or not re.fullmatch(r"[A-Za-z][A-Za-z0-9_.-]{0,127}", value["code"]):
            raise ValueError("invalid diagnostic code")
        _text(value["message"])
        if "primary" in value:
            _location(value["primary"])
        for related in _list(value.get("related", []), 16):
            _object(related, ("message", "location"))
            _text(related["message"])
            _location(related["location"])
        for fix in _list(value.get("fixes", []), 8):
            _object(fix, ("title", "edits"))
            _text(fix["title"])
            edits = _list(fix["edits"], 16)
            if not edits:
                raise ValueError("diagnostic fixes require at least one edit")
            for edit in edits:
                _object(edit, ("location", "replacement"))
                _location(edit["location"])
                if edit["location"]["source"]["kind"] != "workspace":
                    raise ValueError("diagnostic edits require revision-bound workspace sources")
                _text(edit["replacement"], 16 * 1024, empty=True)
        for attachment in _list(value.get("attachments", []), 16):
            _object(attachment, ("kind", "id"), ("label",))
            if attachment["kind"] not in ("blob", "artifact"):
                raise ValueError("unknown diagnostic attachment kind")
            _id(attachment["id"])
            if "label" in attachment:
                _text(attachment["label"])


def diagnostic_summary(values: list) -> str:
    """Shared model projection; at most eight lines and 4096 UTF-8 bytes."""
    validate_diagnostics(values)
    lines = [f"{v['severity']}[{v['code']}]: " + v["message"].replace("\n", " ").replace("\t", " ")
             for v in values[:8]]
    return "\n".join(lines).encode("utf-8")[:4096].decode("utf-8", errors="ignore")


def with_diagnostics(result: dict, diagnostics) -> dict:
    """Add validated diagnostics and explicit model text to an ordinary envelope."""
    if not isinstance(diagnostics, (list, tuple)) or len(diagnostics) > 32:
        raise ValueError("diagnostics must be a bounded list or tuple")
    values = [v.to_wire() if isinstance(v, Diagnostic) else copy.deepcopy(v) for v in diagnostics]
    summary = diagnostic_summary(values)
    metadata = dict(result.get("metadata") or {})
    if DIAGNOSTICS_KEY in metadata:
        raise ValueError("reserved diagnostic metadata already supplied")
    metadata[DIAGNOSTICS_KEY] = values
    content = list(result.get("content", []))
    if summary:
        content.append({"type": "text", "text": summary})
    return {**result, "metadata": metadata, "content": content}

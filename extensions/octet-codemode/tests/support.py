"""Shared harness for the real Rust codemode extension.

Every test in this directory drives the actual executable: the direct warm
runner protocol (`test_runner.py`) or the real API 0.4 stdio wire
(`test_adapter.py`). Missing binaries are a failure, never a silent skip.
Python is test tooling only; the extension itself needs neither Python nor Node.
"""
import json
import os
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = ROOT.parents[1]
FRAME_BYTES = 1024 * 1024
IPC_BYTES = 20 * 1024 * 1024
OUTPUT_BYTES = 50 * 1024
VM_HEAP_BYTES = 256 * 1024 * 1024


def run_checked(command, **kwargs):
    result = subprocess.run(command, capture_output=True, text=True, **kwargs)
    if result.returncode:
        raise AssertionError(
            f"command failed ({result.returncode}): {' '.join(map(str, command))}\n"
            f"{result.stdout}{result.stderr}"
        )
    return result


def runner_binary():
    """The built extension executable, building it once when necessary."""
    override = os.environ.get("CODEMODE_RUNNER")
    if override:
        path = Path(override).resolve()
        if not path.is_file():
            raise AssertionError(f"CODEMODE_RUNNER is not a file: {path}")
        return path
    for profile in ("debug", "release"):
        candidate = ROOT / "target" / profile / "octet-codemode"
        if candidate.is_file():
            return candidate
    cargo = shutil.which("cargo")
    if cargo is None:
        raise AssertionError(
            f"Real Rust executable required; build {ROOT} or set CODEMODE_RUNNER"
        )
    # Build into the extension's own target directory so the resolved path is
    # deterministic regardless of an ambient CARGO_TARGET_DIR.
    environment = dict(os.environ, CARGO_TARGET_DIR=str(ROOT / "target"))
    run_checked(
        [cargo, "build", "--locked", "--manifest-path", str(ROOT / "Cargo.toml")],
        cwd=ROOT,
        env=environment,
    )
    built = ROOT / "target/debug/octet-codemode"
    if not built.is_file():
        raise AssertionError(f"cargo build did not produce {built}")
    return built


def dumps(value):
    return json.dumps(value, ensure_ascii=False, allow_nan=False, separators=(",", ":"))


def validate(value, depth=0):
    """The same portable-JSON invariants the wire enforces."""
    import math

    if depth > 32:
        raise ValueError("JSON nesting exceeds 32 levels")
    if isinstance(value, str):
        value.encode("utf-8", "strict")
    elif isinstance(value, float) and not math.isfinite(value):
        raise ValueError("Non-finite JSON number")
    elif isinstance(value, list):
        for child in value:
            validate(child, depth + 1)
    elif isinstance(value, dict):
        for key, child in value.items():
            validate(key, depth + 1)
            validate(child, depth + 1)


def loads(raw):
    value = json.loads(raw)
    validate(value)
    return value


def text(result):
    return "\n".join(part["text"] for part in result["content"] if part["type"] == "text")


def offer(optional=("tool_composition_v1", "request_progress", "artifacts")):
    """The exact feature-negotiated API 0.4 offer the 0.9.0 host sends."""
    return {
        "api_version": "0.4",
        "octet_version": "0.9.0",
        "extension": {"name": "octet-codemode", "version": "0.9.0"},
        "flag_values": [],
        "protocol": {
            "version": "0.4",
            "required_features": ["request_cancellation", "content_parts"],
            "optional_features": list(optional),
            "limits": {
                "max_concurrent_requests": 64,
                "max_message_bytes": FRAME_BYTES,
                "resource_refs_v1": {"max_records": 256, "max_registrations_per_parent": 32},
            },
            "session_snapshot_transport_v1": {
                "profile": "json-chunks.v1",
                "chunk_bytes": 65536,
                "generation_bytes": 536870912,
                "generation_entries": 2097152,
                "owner_views": 64,
                "projection_bytes": 67108864,
                "projections": 64,
                "snapshot_bytes": 268435456,
                "transfers": 64,
                "view_entries": 1048576,
            },
        },
    }

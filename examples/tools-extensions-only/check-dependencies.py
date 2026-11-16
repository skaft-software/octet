#!/usr/bin/env python3
"""Check the resolved Cargo features, parsed modules, and forbidden public API."""
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent
MANIFEST = ROOT / "Cargo.toml"


def cargo(*args):
    return subprocess.run(
        ["cargo", *args, "--manifest-path", str(MANIFEST), "--locked"],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    ).stdout


metadata = json.loads(cargo("metadata", "--format-version=1"))
packages = {package["id"]: package for package in metadata["packages"]}
for node in metadata["resolve"]["nodes"]:
    name = packages[node["id"]]["name"]
    assert name not in {"octet-coding-agent", "sexy-tui-rs"}, name
    if name == "octet-agent":
        assert set(node["features"]) == {"tools", "extensions"}, node["features"]

artifacts = [
    json.loads(line)
    for line in cargo("check", "--message-format=json").splitlines()
]
agent = next(
    artifact for artifact in artifacts
    if artifact.get("reason") == "compiler-artifact"
    and artifact["target"]["name"] == "octet_agent"
)
assert set(agent["features"]) == {"tools", "extensions"}, agent["features"]
metadata_file = next(Path(name) for name in agent["filenames"] if name.endswith(".rmeta"))
depinfo = metadata_file.with_name(metadata_file.stem.removeprefix("lib") + ".d")
parsed = depinfo.read_text()
excluded = [
    "agent", "session", "session_leaf", "session_writer", "compaction", "delegation",
    "extension_runtime", "cache", "cache_warmer", "context", "telemetry", "goal_driver", "goal_store", "skills",
]
for module in excluded:
    assert f"src/{module}.rs" not in parsed, module
    assert f"src/{module}/" not in parsed, module
assert "extension_process/provider_context.rs" not in parsed
assert "extension_process/session_leaf.rs" not in parsed
assert "extension_process/session_control/replacement.rs" not in parsed
assert "tools/deferred.rs" not in parsed
assert "tools/summarization.rs" not in parsed
print("PASS: only tools/extensions features; no agent/session implementation modules")
# Each compile-fail block imports exactly one forbidden root type or module.
print(cargo("test"), end="")

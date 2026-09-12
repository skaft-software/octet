#!/usr/bin/env python3
"""Check live models.dev metadata before a version change freezes release source.

Ordinary same-version CI is offline for this gate. This script only compares
reviewed snapshots; refreshes and review remain explicit maintainer operations.
It is not part of compilation or the historical signed-release workflows.
"""

from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parent.parent
REFRESH = "scripts/refresh-models-dev-pricing.py"


def workspace_version(manifest: str, source: str) -> str:
    try:
        version = tomllib.loads(manifest)["workspace"]["package"]["version"]
    except (tomllib.TOMLDecodeError, KeyError, TypeError) as error:
        raise ValueError(f"cannot read workspace.package.version from {source}") from error
    if not isinstance(version, str) or not version.strip():
        raise ValueError(f"invalid workspace.package.version in {source}")
    return version


def check_release_metadata(event_name: str, base_sha: str) -> None:
    if event_name not in {"pull_request", "push", "workflow_dispatch"}:
        raise ValueError("unsupported CI event; expected pull_request, push or workflow_dispatch")

    current = workspace_version((ROOT / "Cargo.toml").read_text(), "current Cargo.toml")
    if event_name == "workflow_dispatch":
        reason = f"manual dispatch for {current}"
    else:
        # Never accept a ref, revision expression, shell fragment or missing base.
        if not re.fullmatch(r"[0-9a-fA-F]{40}", base_sha) or base_sha == "0" * 40:
            raise ValueError("MODEL_METADATA_BASE_SHA must be a nonzero 40-hex trusted event SHA")
        try:
            base_manifest = subprocess.run(
                ["git", "show", f"{base_sha}:Cargo.toml"],
                cwd=ROOT, check=True, capture_output=True, text=True, timeout=10,
            ).stdout
        except subprocess.TimeoutExpired as error:
            raise ValueError(
                "reading trusted event base Cargo.toml exceeded the 10-second deadline"
            ) from error
        except subprocess.CalledProcessError as error:
            raise ValueError(
                "cannot read Cargo.toml at the trusted event base SHA; "
                "fetch full history (checkout fetch-depth: 0) and verify the event base"
            ) from error
        previous = workspace_version(base_manifest, "base Cargo.toml")
        if current == previous:
            print(f"Workspace version remains {current}; skipping live models.dev check.")
            return
        reason = f"workspace version changed from {previous} to {current}"

    print(f"Checking current models.dev metadata: {reason}.", flush=True)
    try:
        # No --source override: compare all checked-in outputs and the receipt
        # against the public live snapshot, without writing any generated data.
        # Bound the whole command: an HTTP socket timeout alone permits a
        # trickling response to keep the freshness check alive indefinitely.
        subprocess.run([sys.executable, REFRESH, "--check"],
                       cwd=ROOT, check=True, timeout=60)
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        raise ValueError(
            "models.dev metadata is stale, unavailable or timed out (60-second deadline). "
            f"Rerun `python3 {REFRESH}` when models.dev is available, "
            "review the names, pricing, capabilities and source receipt together, "
            "then rerun CI before freezing release source. CI does not update snapshots."
        ) from error


def main() -> int:
    try:
        check_release_metadata(
            os.environ.get("GITHUB_EVENT_NAME", ""),
            os.environ.get("MODEL_METADATA_BASE_SHA", ""),
        )
    except (OSError, ValueError) as error:
        print(f"models.dev pre-release freshness gate failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

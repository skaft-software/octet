#!/usr/bin/env python3
"""Small, credential-free contributor smoke lane; never installs dependencies."""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import shlex
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def checks() -> list[tuple[str, list[str]]]:
    return [
        ("effect admission, file edits, private files and ephemeral bulk storage", [
            "cargo", "test", "-p", "octet-agent", "--lib", "--locked", "--offline", "--",
            "effect::tests", "tools::edit::tests", "secure_fs::tests", "extension_bulk::tests",
            "extension_bulk_host::tests::temporary_storage_never_promises_durability",
        ]),
        ("real agent-dispatch shell approvals", [
            "cargo", "test", "-p", "octet-agent", "--test", "agent_run", "--locked", "--offline",
            "--", "controlled_shell_",
        ]),
        ("Python SDK (platform/runtime skips explain their reasons)", [
            sys.executable, "-m", "unittest", "discover", "-s", "sdk/python/tests", "-v",
        ]),
        ("patch whitespace", ["git", "diff", "--check"]),
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--list", action="store_true", help="show commands without executing them")
    args = parser.parse_args()
    environment = os.environ.copy()
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    environment["PYTHONPATH"] = str(ROOT / "sdk/python")
    failed = False
    for label, command in checks():
        print(f"\n{label}\n  {shlex.join(command)}", flush=True)
        if args.list:
            continue
        try:
            result = subprocess.run(command, cwd=ROOT, env=environment, timeout=1200)
            status = result.returncode
        except (OSError, subprocess.TimeoutExpired) as error:
            print(f"FAIL: {error}", file=sys.stderr)
            status = 1
        failed |= status != 0
        print(f"{'PASS' if status == 0 else 'FAIL'}: {label}", flush=True)
    if not args.list:
        print("\nSelected offline smoke checks only; not full CI, release or live-provider qualification.")
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())

"""Prerequisite-fenced F01/F02 production-host command. Never installs anything."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

from solver import SpiceError, require_ngspice

HERE = Path(__file__).resolve().parent
SOURCE = HERE.parents[2]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path, help="parent-approved shared Cargo target")
    parser.add_argument("--run-host", action="store_true", help="build/run only with the parent's build slot")
    parser.add_argument("--evidence", type=Path, help="retain bounded host JSON evidence and source identities")
    args = parser.parse_args()
    try:
        require_ngspice()
    except SpiceError as error:
        for case in ("F01 spice_acceptance", "F02 spice_interrupt"):
            print(f"{case}: {error}", file=sys.stderr)
        return 2
    if not args.run_host or args.target_dir is None:
        print("BLOCKED F01/F02: explicit --run-host and parent-approved --target-dir required.", file=sys.stderr)
        return 2
    cargo = shutil.which("cargo")
    if cargo is None:
        print("BLOCKED F01/F02: Cargo is missing; no installation attempted.", file=sys.stderr)
        return 2
    env = {**os.environ, "CARGO_TARGET_DIR": str(args.target_dir.resolve()),
           "CARGO_BUILD_JOBS": "1", "PYTHONDONTWRITEBYTECODE": "1"}
    if args.evidence is not None:
        root = args.evidence.resolve()
        root.mkdir(parents=True, exist_ok=False)
        env["OCTET_SPICE_EVIDENCE_DIR"] = str(root)
        # Snapshot source identities, not secrets, runtime payloads or OS binaries.
        paths = [*HERE.glob("*.py"), HERE / "rc.cir", HERE / "host-smoke/Cargo.toml",
                 HERE / "host-smoke/Cargo.lock", HERE / "host-smoke/tests/spice.rs",
                 * (SOURCE / "sdk/python/octet_extension").glob("*.py"),
                 * (SOURCE / "crates/octet-agent/src").rglob("*.rs")]
        hashes = {str(path.relative_to(SOURCE)): hashlib.sha256(path.read_bytes()).hexdigest()
                  for path in sorted(paths)}
        (root / "source-sha256.json").write_text(json.dumps(hashes, indent=2) + "\n")
    # Resolve only the checked-in lockfile from the local cache; never download.
    command = [cargo, "test", "--offline", "--locked", "--jobs", "1", "--profile", "ci-test",
               "--manifest-path", str(HERE / "host-smoke/Cargo.toml"),
               "--test", "spice", "--", "--nocapture", "--test-threads=1"]
    print("Host command:", json.dumps(command), flush=True)
    return subprocess.run(command, cwd=SOURCE, env=env, check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())

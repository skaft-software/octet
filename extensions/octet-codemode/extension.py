#!/usr/bin/env python3
"""Staging-safe stdlib bootstrap; all runtime dependencies are in this bundle."""
from pathlib import Path
import os
import shutil
import sys


def main():
    root = Path(os.environ.get("OCTET_EXTENSION_DIR", Path(__file__).resolve().parent)).resolve()
    node = shutil.which("node")
    if not node:
        print("octet-codemode requires Node.js >=22.19.0 on PATH; install Node and reload the extension (no npm install is needed).", file=sys.stderr)
        return 1
    # Never search for scripts or npm packages in the active workspace. Node's
    # ambient preload flags are not an extension API and must not be inherited.
    environment = dict(os.environ)
    environment.pop("NODE_OPTIONS", None)
    environment.pop("NODE_PATH", None)
    os.execve(node, [node, str(root / "runtime.mjs")], environment)


if __name__ == "__main__":
    raise SystemExit(main())

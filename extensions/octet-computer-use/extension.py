#!/usr/bin/env python3
"""Self-contained API 0.4 entry point for the official octet computer-use bundle.

The bundle connects octet to a locally installed MIT-licensed Cua Driver
(``trycua/cua``) over stdio MCP. It provisions the driver on explicit request,
never installs operating-system permissions, and requires user confirmation for
any driver action the driver does not declare read-only.
"""

from pathlib import Path
import os
import sys

ROOT = Path(os.environ.get("OCTET_EXTENSION_DIR", Path(__file__).resolve().parent)).resolve()
sys.path.insert(0, str(ROOT / "vendor"))
sys.path.insert(0, str(ROOT))

from octet_computer_use.entrypoint import main  # noqa: E402


if __name__ == "__main__":
    main()

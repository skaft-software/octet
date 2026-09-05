#!/usr/bin/env python3
"""Self-contained API 0.2 entrypoint for the official octet Browse bundle."""

from pathlib import Path
import os
import sys

ROOT = Path(os.environ.get("OCTET_EXTENSION_DIR", Path(__file__).resolve().parent)).resolve()
sys.path.insert(0, str(ROOT / "vendor"))
sys.path.insert(0, str(ROOT))

from octet_browse.runtime import main  # noqa: E402


if __name__ == "__main__":
    main()

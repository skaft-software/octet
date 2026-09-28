"""Forward the runners' visual mode through upstream's managed verifier.

Executed only by jev_use with the pinned recipe as cwd and its locked Python.
The upstream verifier's fixture, independent readback and cleanup stay intact.
No user source, arbitrary import name, or executable argument is accepted.
"""
from __future__ import annotations

import argparse
import importlib.util
from pathlib import Path
import sys


def main() -> None:
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--visual-observation", choices=("auto", "always", "off"), default="auto")
    options, arguments = parser.parse_known_args()
    root = Path.cwd()
    sys.path.insert(0, str(root))
    spec = importlib.util.spec_from_file_location("upstream_jev_verifier", root / "verify_setup.py")
    verifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(verifier)
    original = verifier.runner_command

    def runner_command(language: str, provider: str) -> list[str]:
        return original(language, provider) + ["--visual-observation", options.visual_observation]

    verifier.runner_command = runner_command
    sys.argv = [str(root / "verify_setup.py"), *arguments]
    verifier.main()


if __name__ == "__main__":
    main()

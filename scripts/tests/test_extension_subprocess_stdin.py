"""Hosted extension code never lets a child inherit the protocol stdin.

An executable extension reads JSON-RPC from stdin on a reader thread. On
Windows that is a synchronous pipe, and a child that inherits it can stall in
C runtime startup until the host sends another message. That hung the
computer-use status probe and options menu only when hosted, so every
subprocess call in extension and SDK code must choose its stdin explicitly.
"""

from __future__ import annotations

import ast
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCANNED = (ROOT / "extensions", ROOT / "sdk" / "python")
# Test code runs under a test runner, not as a hosted extension.
SKIPPED_PARTS = {"tests", "fixtures", "__pycache__", "node_modules", "target", ".venv"}
SPAWNERS = {"run", "Popen", "call", "check_call", "check_output"}
ASYNC_SPAWNERS = {"create_subprocess_exec", "create_subprocess_shell"}


def _sources():
    for base in SCANNED:
        for path in sorted(base.rglob("*.py")):
            relative = path.relative_to(ROOT)
            if SKIPPED_PARTS.intersection(relative.parts) or path.name.startswith("test_"):
                continue
            yield path


def _spawn_name(call: ast.Call, imported: set) -> str | None:
    func = call.func
    if isinstance(func, ast.Attribute) and isinstance(func.value, ast.Name):
        if func.value.id == "subprocess" and func.attr in SPAWNERS:
            return f"subprocess.{func.attr}"
        if func.value.id == "asyncio" and func.attr in ASYNC_SPAWNERS:
            return f"asyncio.{func.attr}"
    if isinstance(func, ast.Name) and func.id in imported:
        return func.id
    return None


def unguarded_spawns(source: str) -> list:
    tree = ast.parse(source)
    imported = {
        alias.asname or alias.name
        for node in ast.walk(tree)
        if isinstance(node, ast.ImportFrom) and node.module == "subprocess"
        for alias in node.names
        if alias.name in SPAWNERS
    }
    found = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        name = _spawn_name(node, imported)
        if name is None:
            continue
        keywords = {keyword.arg for keyword in node.keywords}
        # `input=` makes subprocess.run supply its own stdin pipe.
        if not keywords & {"stdin", "input"}:
            found.append((node.lineno, name))
    return found


class ExtensionSubprocessStdinTests(unittest.TestCase):
    def test_every_hosted_spawn_chooses_its_stdin(self):
        offenders = []
        for path in _sources():
            for line, name in unguarded_spawns(path.read_text(encoding="utf-8")):
                offenders.append(f"{path.relative_to(ROOT)}:{line} {name}")
        self.assertEqual(
            offenders,
            [],
            "pass stdin=subprocess.DEVNULL (or a pipe) so the child never inherits "
            "the extension's protocol stdin",
        )

    def test_the_guard_catches_an_inherited_stdin(self):
        source = (
            "import subprocess\n"
            "from subprocess import Popen as Spawn\n"
            "subprocess.run(['x'], capture_output=True)\n"
            "Spawn(['x'], stdout=subprocess.PIPE)\n"
            "subprocess.run(['x'], stdin=subprocess.DEVNULL)\n"
            "subprocess.run(['x'], input=b'')\n"
        )
        self.assertEqual(unguarded_spawns(source), [(3, "subprocess.run"), (4, "Spawn")])


if __name__ == "__main__":
    unittest.main()

"""Integration coverage that runs against a real, locally installed Cua Driver.

These tests are skipped unless a driver is already provisioned. They never
provision implicitly and never grant an operating-system permission, so a
developer's machine is only read from, never driven.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import unittest
from pathlib import Path


def _probe_binary() -> str | None:
    """Return a usable driver binary, or None when none is available."""

    override = os.environ.get("OCTET_CUA_DRIVER_BINARY")
    if override and Path(override).is_file():
        return override
    found = shutil.which("cua-driver")
    if found:
        return found
    try:
        completed = subprocess.run(
            [sys.executable, "-c", "import cua_driver; print(cua_driver.get_binary_path())"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if completed.returncode != 0:
        return None
    candidate = Path((completed.stdout or "").strip())
    return str(candidate) if candidate.is_file() else None


BINARY = _probe_binary()


@unittest.skipIf(BINARY is None, "no locally installed Cua Driver")
class LiveDriverTests(unittest.TestCase):
    """Verify the handshake and the read-only classification against a real driver."""

    def test_handshake_publishes_a_catalog_and_classifies_read_only(self):
        from octet_computer_use.driver_client import DriverClient

        client = DriverClient(BINARY)
        client.start()
        try:
            self.assertTrue(client.started)
            tools = client.tools()
            self.assertGreater(len(tools), 20)

            by_name = {info.name: info for info in tools}
            # Observation tools the skill depends on must be present and read-only.
            for name in ("list_apps", "list_windows", "get_window_state"):
                self.assertIn(name, by_name, f"{name} is missing from the live catalog")
                self.assertTrue(
                    by_name[name].read_only,
                    f"{name} should be annotated readOnlyHint",
                )
            # Mutating tools must never be classified read-only.
            for name in ("click", "type_text", "launch_app", "kill_app"):
                if name in by_name:
                    self.assertFalse(by_name[name].read_only, f"{name} must be gated")
        finally:
            client.close()
        self.assertFalse(client.started)

    def test_unknown_tool_is_gated_and_known_read_only_is_not(self):
        from octet_computer_use.driver_client import DriverClient

        client = DriverClient(BINARY)
        client.start()
        try:
            self.assertFalse(client.requires_confirmation("get_window_state"))
            self.assertTrue(client.requires_confirmation("click"))
            self.assertTrue(client.requires_confirmation("definitely_not_a_tool"))
        finally:
            client.close()

    def test_list_apps_is_callable_without_confirmation(self):
        from octet_computer_use.driver_client import DriverClient
        from octet_computer_use.service import summarize_result

        client = DriverClient(BINARY)
        client.start()
        try:
            self.assertFalse(client.requires_confirmation("list_apps"))
            result = client.call("list_apps", {})
            summary = summarize_result(result)
            self.assertIsInstance(summary["text"], str)
            self.assertGreater(len(summary["text"]), 0)
        finally:
            client.close()


if __name__ == "__main__":
    unittest.main()

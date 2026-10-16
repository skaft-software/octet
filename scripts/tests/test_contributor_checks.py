"""The contributor lane stays bounded, offline and explicit about its scope."""
import contextlib
import importlib.util
import io
from pathlib import Path
import unittest
from unittest.mock import patch

PATH = Path(__file__).resolve().parents[1] / "check-contributor.py"
SPEC = importlib.util.spec_from_file_location("contributor_checks", PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ContributorChecksTests(unittest.TestCase):
    def test_rust_checks_are_locked_offline_and_include_agent_dispatch(self):
        commands = [command for _, command in MODULE.checks()]
        rust = [command for command in commands if command[0] == "cargo"]
        self.assertEqual(len(rust), 2)
        for command in rust:
            self.assertIn("--locked", command)
            self.assertIn("--offline", command)
        self.assertIn("controlled_shell_", rust[1])
        self.assertIn("extension_bulk::tests", rust[0])
        self.assertNotIn("extension_bulk::durable", rust[0])
        sdk = next(command for command in commands if "sdk/python/tests" in command)
        self.assertIn("-v", sdk, "platform/runtime skip reasons must be visible")

    def test_failures_are_reported_without_skipping_independent_checks(self):
        with patch.object(MODULE.sys, "argv", [str(PATH)]), \
                patch.object(MODULE, "checks", return_value=[("bad", ["bad"]), ("good", ["good"])]), \
                patch.object(MODULE.subprocess, "run") as run, contextlib.redirect_stdout(io.StringIO()):
            run.side_effect = [MODULE.subprocess.CompletedProcess(["bad"], 1),
                               MODULE.subprocess.CompletedProcess(["good"], 0)]
            self.assertEqual(MODULE.main(), 1)
            self.assertEqual(run.call_count, 2)
            self.assertEqual(run.call_args.kwargs["timeout"], 1200)

    def test_list_does_not_launch_any_command(self):
        with patch.object(MODULE.sys, "argv", [str(PATH), "--list"]), \
                patch.object(MODULE.subprocess, "run") as run, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(MODULE.main(), 0)
            run.assert_not_called()


if __name__ == "__main__":
    unittest.main()

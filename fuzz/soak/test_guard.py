import os
import tempfile
import unittest
from pathlib import Path

import guard


class GuardTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="soak-guard-")).resolve()
        home = self.root / "home"
        self.env = {"HOME": str(home), "OCTET_PI_AGENT_DIR": str(home / ".pi" / "agent")}

    def test_isolated_environment_passes(self):
        guard.check_isolated(self.env, self.root)

    def test_real_home_is_refused(self):
        with self.assertRaises(guard.IsolationError):
            guard.check_isolated({**self.env, "HOME": str(guard.real_home())}, self.root)

    def test_pi_agent_dir_outside_is_refused(self):
        for name in ("OCTET_PI_AGENT_DIR", "PI_CODING_AGENT_DIR"):
            with self.assertRaises(guard.IsolationError):
                guard.check_isolated({**self.env, name: str(guard.real_home() / ".pi" / "agent")}, self.root)

    def test_octet_dir_outside_is_refused(self):
        with self.assertRaises(guard.IsolationError):
            guard.check_isolated({**self.env, "OCTET_DATA_DIR": "/var/tmp/elsewhere"}, self.root)

    def test_symlink_escape_is_refused(self):
        (self.root / "home").mkdir()
        (self.root / "home" / ".pi").symlink_to(guard.real_home() / ".pi")
        with self.assertRaises(guard.IsolationError):
            guard.check_isolated(self.env, self.root)

    def test_missing_home_and_run_dir_in_real_home_are_refused(self):
        with self.assertRaises(guard.IsolationError):
            guard.check_isolated({}, self.root)
        with self.assertRaises(guard.IsolationError):
            guard.check_isolated(self.env, guard.real_home() / "soak-run")
        with self.assertRaises(guard.IsolationError):
            guard.preflight(guard.real_home() / "tmp")


if __name__ == "__main__":
    unittest.main()

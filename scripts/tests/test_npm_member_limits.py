"""Size budgets for native-to-npm release packaging."""
from pathlib import Path
import runpy
import unittest


class MemberLimitTests(unittest.TestCase):
    def test_only_windows_cli_gets_the_larger_member_budget(self):
        verify = runpy.run_path(str(Path(__file__).resolve().parents[1] / "verify-octet-npm.py"))
        limit = verify["member_limit"]
        self.assertEqual(limit(True, "octet.exe"), 80 << 20)
        for windows, path in [(False, "octet.exe"), (True, "octet-host.exe"),
                              (True, "docs/octet.exe"), (False, "octet")]:
            with self.subTest(windows=windows, path=path):
                self.assertEqual(limit(windows, path), 64 << 20)
        self.assertEqual(verify["MAX_EXPANDED_BYTES"], 160 << 20)

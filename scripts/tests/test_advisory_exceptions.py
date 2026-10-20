"""Keep advisory exceptions as narrow as their recorded rationale."""

import tomllib
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def _locked_versions(name):
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    return [tuple(int(part) for part in package["version"].split("."))
            for package in lock["package"] if package["name"] == name]


class AdvisoryExceptionTests(unittest.TestCase):
    def test_cargo_audit_and_cargo_deny_ignore_the_same_advisories(self):
        deny = tomllib.loads((ROOT / "deny.toml").read_text())["advisories"]["ignore"]
        audit = tomllib.loads((ROOT / ".cargo/audit.toml").read_text())["advisories"]["ignore"]
        # Unmaintained warnings fail only cargo-deny; vulnerabilities fail both.
        self.assertLessEqual(set(audit), set(deny))
        self.assertIn("RUSTSEC-2026-0381", audit)

    def test_webpki_roots_exception_covers_only_the_compatibility_shim(self):
        # RUSTSEC-2026-0381 is fixed in webpki-roots 1.0.10. The ignore is by ID,
        # so it must not hide a vulnerable 1.x; the 0.26.11 shim re-exports 1.x.
        versions = _locked_versions("webpki-roots")
        current = [v for v in versions if v[0] >= 1]
        shims = [v for v in versions if v[0] == 0]
        self.assertTrue(current)
        for version in current:
            self.assertGreaterEqual(version, (1, 0, 10))
        self.assertEqual(shims, [(0, 26, 11)])


if __name__ == "__main__":
    unittest.main()

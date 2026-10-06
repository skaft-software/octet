"""Exercise typed warming advice through each bundled Python SDK."""

import os
from pathlib import Path
import subprocess
import sys
import unittest


REPOSITORY = Path(__file__).resolve().parents[3]
SDK_TESTS = REPOSITORY / "sdk" / "python" / "tests"


class CacheWarmingBundleTests(unittest.TestCase):
    def test_official_bundles_support_typed_warming_advice(self):
        catalog = REPOSITORY / "extensions" / "release-catalog.txt"
        for line in catalog.read_text().splitlines():
            name = line.strip()
            if not name or name.startswith("#"):
                continue
            with self.subTest(bundle=name):
                bundle = REPOSITORY / "extensions" / name
                sdk_root = bundle / "vendor" if (bundle / "vendor").is_dir() else bundle
                if not (sdk_root / "octet_extension").is_dir():
                    self.skipTest("non-Python protocol runtime does not bundle the Python SDK")
                environment = os.environ.copy()
                environment["PYTHONPATH"] = str(sdk_root)
                environment["PYTHONDONTWRITEBYTECODE"] = "1"
                result = subprocess.run(
                    [
                        sys.executable, "-m", "unittest", "discover",
                        "-s", str(SDK_TESTS), "-p", "test_cache_warming.py",
                    ],
                    cwd=REPOSITORY,
                    env=environment,
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

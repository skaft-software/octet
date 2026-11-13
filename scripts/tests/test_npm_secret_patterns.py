"""The npm secret scanner matches real token formats, not compiled string tables."""

import importlib.util
from pathlib import Path
import sys
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("verify_octet_npm", SCRIPTS / "verify-octet-npm.py")
verify = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = verify
SPEC.loader.exec_module(verify)


def flagged(data: bytes) -> bool:
    return any(pattern.search(data) for pattern in verify.SECRET_PATTERNS)


class SecretPatternTests(unittest.TestCase):
    def test_real_token_formats_are_flagged(self):
        body = b"A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"
        self.assertEqual(len(body), 36)
        for prefix in (b"ghp_", b"gho_", b"ghu_", b"ghs_", b"ghr_", b"npm_"):
            with self.subTest(prefix=prefix):
                self.assertTrue(flagged(b"token=" + prefix + body + b"\n"))
                self.assertTrue(flagged(b"\x00" + prefix + body + b"\x00"))
        self.assertTrue(flagged(b"-----BEGIN PRIVATE KEY-----"))
        self.assertTrue(flagged(b"AKIAABCDEFGHIJKLMNOP"))

    def test_compiled_prefix_vocabulary_is_not_a_token(self):
        # Observed in the 0.10.4 Darwin host: redaction prefixes packed beside
        # unrelated identifiers in the binary string table.
        table = (b"$refghp-ghp_gho-gho_ghu-ghu_ghs-ghs_JsonbytecrlsAuthHttpPongtimehavewantrsv1"
                 b"rsv2rsv3maskUtf8DenyPureAwayIana\x00\x00")
        self.assertFalse(flagged(table))

    def test_wrong_length_bodies_are_not_tokens(self):
        for body in (b"A1b2C3d4E5f6G7h8I9j0", b"A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9"):
            self.assertFalse(flagged(b" ghp_" + body + b" "))


if __name__ == "__main__":
    unittest.main()

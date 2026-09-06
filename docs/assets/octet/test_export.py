#!/usr/bin/env python3
"""Regression checks for the octet identity export (standard library only)."""

import io
import unittest
import zipfile

from export import ROOT, artifacts, validate_png, validate_svg, validated_payload


class IdentityExportTests(unittest.TestCase):
    def test_pinned_assets_and_reproducible_package(self):
        payload = validated_payload()
        first = artifacts(payload)
        self.assertEqual(first, artifacts(dict(reversed(list(payload.items())))))
        with zipfile.ZipFile(io.BytesIO(first["octet-identity.zip"])) as archive:
            self.assertEqual(archive.namelist(), sorted(archive.namelist()))
            self.assertIsNone(archive.testzip())
            for entry in archive.infolist():
                self.assertEqual(entry.date_time, (1980, 1, 1, 0, 0, 0))
                self.assertNotIn("preview", entry.filename)
                self.assertNotIn("social-card", entry.filename)

    def test_wrong_bit_and_gap_rejected(self):
        source = (ROOT / "marks/mark-gradient.svg").read_bytes()
        with self.assertRaisesRegex(ValueError, "wrong byte"):
            validate_svg(source.replace(b'data-value="0"', b'data-value="1"', 1),
                         "mark-gradient.svg")
        with self.assertRaisesRegex(ValueError, "non-contiguous"):
            validate_svg(source.replace(b'x="32.0"', b'x="33.0"', 1),
                         "mark-gradient.svg")

    def test_wordmark_must_be_outlined(self):
        source = (ROOT / "marks/wordmark-black.svg").read_bytes()
        with self.assertRaisesRegex(ValueError, "non-outlined"):
            validate_svg(source.replace(b"</svg>", b"<text>octet</text></svg>"),
                         "wordmark-black.svg")

    def test_small_icons_and_corrupt_png(self):
        for surface in ("dark", "light"):
            for size in (16, 24, 32):
                name = f"icon-{surface}-{size}.png"
                source = (ROOT / "marks" / name).read_bytes()
                self.assertEqual(validate_png(source, name), (size, size))
                corrupt = bytearray(source)
                corrupt[20] ^= 1
                with self.assertRaisesRegex(ValueError, "PNG CRC"):
                    validate_png(corrupt, name)


if __name__ == "__main__":
    unittest.main()

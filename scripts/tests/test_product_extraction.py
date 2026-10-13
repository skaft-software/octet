"""Offline regression checks for the pinned product source verifier."""
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import unittest
from contextlib import redirect_stdout
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "verify-product-extraction.py"
spec = importlib.util.spec_from_file_location("product_extraction", SCRIPT)
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)


class ProductExtractionTests(unittest.TestCase):
    def setUp(self):
        self.manifest = {"products": [{
            "name": "fixture", "source_revision": "source-revision",
            "published_revision": "published-revision",
            "source_path": "extensions/fixture", "files": 1,
        }]}
        self.original = b"#!/bin/sh\nprintf hello\n"
        self.extracted = self.original
        self.mode = "100755"
        self.head = b"published-revision\n"
        self.provenance = {
            "revision": "source-revision", "source_path": "extensions/fixture",
            "files": [{"path": "entrypoint", "source_path": "extensions/fixture/entrypoint",
                       "mode": "100755", "sha256": hashlib.sha256(self.original).hexdigest()}],
        }

    def git(self, root, *args):
        if args[0] == "rev-parse":
            return self.head
        if args[:2] == ("ls-tree", "-rz"):
            return b"100755 blob object-id\textensions/fixture/entrypoint\0"
        if args[:2] == ("cat-file", "blob"):
            return self.original
        if args[0] == "show":
            if args[1].endswith(":MIGRATION-SOURCE.json"):
                return json.dumps(self.provenance).encode()
            return self.extracted
        if args[0] == "ls-tree":
            return f"{self.mode} blob object-id\tentrypoint\n".encode()
        raise AssertionError(args)

    def verify(self):
        with patch.object(verifier, "git", side_effect=self.git), redirect_stdout(io.StringIO()):
            return verifier.verify(Path("source"), Path("products"), self.manifest)

    def test_complete_identical_snapshot_passes(self):
        self.assertEqual(self.verify(), 1)

    def test_changed_source_bytes_are_rejected(self):
        self.extracted = b"changed"
        with self.assertRaisesRegex(ValueError, "content mismatch"):
            self.verify()

    def test_missing_executable_permission_is_rejected(self):
        self.mode = "100644"
        with self.assertRaisesRegex(ValueError, "mode mismatch"):
            self.verify()

    def test_unpinned_branch_tip_is_rejected(self):
        self.head = b"moving-tip\n"
        with self.assertRaisesRegex(ValueError, "pinned revision"):
            self.verify()

    def test_incomplete_and_duplicate_inventories_are_rejected(self):
        original = copy.deepcopy(self.provenance)
        for entries in ([], original["files"] * 2):
            self.provenance["files"] = entries
            with self.subTest(entries=entries), self.assertRaisesRegex(ValueError, "incomplete"):
                self.verify()

    def test_wrong_upstream_provenance_is_rejected(self):
        self.provenance["revision"] = "other-revision"
        with self.assertRaisesRegex(ValueError, "provenance source mismatch"):
            self.verify()

    def test_corrupt_hash_is_rejected(self):
        self.provenance["files"][0]["sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "content mismatch"):
            self.verify()


if __name__ == "__main__":
    unittest.main()

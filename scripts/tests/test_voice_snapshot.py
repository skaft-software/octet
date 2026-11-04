"""Verify the tracked voice snapshot's Git tree and working bytes, offline."""
import hashlib
import json
from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]


def object_hash(kind, contents):
    return hashlib.sha1(kind + b" " + str(len(contents)).encode() + b"\0" + contents).hexdigest()


def tree_hash(directory):
    entries = []
    for name, value in directory.items():
        name_bytes = name.encode("utf-8")
        if isinstance(value, dict):
            entries.append((name_bytes + b"/", b"40000", name_bytes, tree_hash(value)))
        else:
            mode, oid = value
            entries.append((name_bytes, mode.encode(), name_bytes, oid))
    body = b"".join(mode + b" " + name + b"\0" + bytes.fromhex(oid)
                    for _, mode, name, oid in sorted(entries))
    return object_hash(b"tree", body)


class VoiceSnapshotTests(unittest.TestCase):
    def test_snapshot_matches_the_pinned_git_tree(self):
        pin = json.loads((ROOT / "extensions/octet-voice-source.json").read_text())
        self.assertEqual(pin["repository"], "https://github.com/skaft-software/octet-voice")
        self.assertEqual(pin["snapshot"], "extensions/octet-voice")
        for key in ("commit", "tree"):
            self.assertRegex(pin[key], r"^[0-9a-f]{40}$")
        prefix = pin["snapshot"] + "/"
        records = subprocess.check_output(
            ["git", "-C", str(ROOT), "ls-files", "--stage", "-z", "--", pin["snapshot"]]
        ).split(b"\0")
        tree = {}
        count = 0
        for record in filter(None, records):
            metadata, raw_path = record.split(b"\t", 1)
            mode, oid, stage = metadata.decode().split()
            path = raw_path.decode("utf-8")
            self.assertIn(mode, ("100644", "100755"))
            self.assertEqual(stage, "0")
            self.assertTrue(path.startswith(prefix))
            file = ROOT / path
            self.assertTrue(file.is_file() and not file.is_symlink(), path)
            self.assertEqual(object_hash(b"blob", file.read_bytes()), oid, path)
            parts = path.removeprefix(prefix).split("/")
            parent = tree
            for part in parts[:-1]:
                parent = parent.setdefault(part, {})
            self.assertNotIn(parts[-1], parent)
            parent[parts[-1]] = (mode, oid)
            count += 1
        self.assertGreater(count, 0)
        self.assertEqual(tree_hash(tree), pin["tree"])


if __name__ == "__main__":
    unittest.main()

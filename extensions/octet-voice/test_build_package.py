"""Source-only package privacy, closure and reproducibility; no runtime setup."""
import importlib.util
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("voice_package_builder", ROOT / "build-package.py")
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


class PackageTests(unittest.TestCase):
    def test_reproducible_archive_allowlist_and_exclusive_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory)
            source = parent / "source"
            source.mkdir()
            for name in builder.FILES:
                shutil.copyfile(ROOT / name, source / name)
            (source / "vendor/octet_extension").mkdir(parents=True)
            for name in builder.SDK_FILES:
                shutil.copyfile(ROOT / "vendor/octet_extension" / name,
                                source / "vendor/octet_extension" / name)
            # Even Python files inside vendor are excluded unless reviewed explicitly.
            for relative in ("voice.json", ".env", "audio.wav", "vendor/octet_extension/private.py"):
                (source / relative).write_text("PRIVATE FIXTURE: MUST NOT SHIP", encoding="utf-8")
            with patch.object(builder, "ROOT", source):
                first, second = parent / "one.tar.gz", parent / "two.tar.gz"
                builder.build(parent / "one", first)
                builder.build(parent / "two", second)
                self.assertEqual(first.read_bytes(), second.read_bytes())
                with self.assertRaises(FileExistsError):
                    builder.build(parent / "one")
                with self.assertRaises(FileExistsError):
                    builder.build(parent / "three", first)
            with tarfile.open(first) as archive:
                members = archive.getmembers()
                self.assertTrue(all(member.isfile() or member.isdir() for member in members))
                names = {member.name for member in members if member.isfile()}
                self.assertEqual(names, {"octet-voice/" + name for name in builder.FILES}
                    | {"octet-voice/vendor/octet_extension/" + name for name in builder.SDK_FILES})
                self.assertEqual(archive.getmember("octet-voice/launcher.py").mode, 0o755)
                for member in members:
                    self.assertEqual((member.uid, member.gid, member.mtime), (0, 0, 0))
                    if member.isfile():
                        self.assertNotIn(b"PRIVATE FIXTURE", archive.extractfile(member).read())

    def test_linked_source_is_rejected_before_staging(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "stage"
            with patch.object(Path, "is_symlink", return_value=True):
                with self.assertRaisesRegex(ValueError, "linked package source"):
                    builder.build(output)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()

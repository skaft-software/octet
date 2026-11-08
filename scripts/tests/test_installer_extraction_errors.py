"""Installer archive-validator diagnostics for local filesystem failures.

A filesystem that refuses an extraction write (quota, space, permissions, I/O)
must be reported as a filesystem failure carrying the errno, not folded into the
"release archive is malformed" verdict. These tests inject one exact errno at
``os.open`` so the classification is deterministic on every host, and they keep
adversarial archive rejection unambiguous.
"""

import errno
import gzip
import hashlib
import io
import os
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tarfile
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
INSTALLER = REPO / "scripts" / "install.sh"
PACKAGE = "octet-1.2.3-fixture"
MASKED_MESSAGE = "release archive is malformed or could not be extracted"

# Raises one chosen errno from the validator's extraction write path only, so a
# host without quotas still exercises the same call and handler.
FAULT_DRIVER = '''\
import os
import runpy
import sys

number = int(os.environ["OCTET_TEST_FAULT_ERRNO"])
suffix = os.environ["OCTET_TEST_FAULT_SUFFIX"]
original_open = os.open


def guarded_open(path, *args, **kwargs):
    target = os.fspath(path)
    if target.endswith(suffix):
        raise OSError(number, os.strerror(number), target)
    return original_open(path, *args, **kwargs)


os.open = guarded_open
validator = sys.argv[1]
sys.argv = sys.argv[1:]
runpy.run_path(validator, run_name="__main__")
'''


def validator_source():
    """Return the archive validator program embedded in install.sh.

    install.sh embeds two Python heredocs; only the one inside
    extract_validated_archive() validates and extracts archive members.
    """
    text = INSTALLER.read_text(encoding="utf-8")
    function = text.index("extract_validated_archive()")
    marker = "<<'PY'\n"
    start = text.index(marker, function) + len(marker)
    end = text.index("\nPY\n", start)
    return text[start:end] + "\n"


def add_directory(output, name):
    info = tarfile.TarInfo(name)
    info.type = tarfile.DIRTYPE
    info.mode = 0o755
    output.addfile(info)


def add_file(output, name, data):
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = 0o644
    output.addfile(info, io.BytesIO(data))


def add_symlink(output, name, target):
    info = tarfile.TarInfo(name)
    info.type = tarfile.SYMTYPE
    info.linkname = target
    output.addfile(info)


FIXTURE_FILES = {
    "README.md": b"# fixture\n",
    "docs/index.md": b"# docs\n",
    "examples/README.md": b"# example\n",
    "sdk/README.md": b"# sdk\n",
}


def write_source_archive(path, variant="valid"):
    directories = [PACKAGE] + sorted({
        f"{PACKAGE}/{parent.as_posix()}"
        for name in FIXTURE_FILES
        for parent in PurePosixPath(name).parents
        if parent.as_posix() != "."
    })
    with path.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as output:
                for directory in directories:
                    add_directory(output, directory)
                for name, data in FIXTURE_FILES.items():
                    add_file(output, f"{PACKAGE}/{name}", data)
                if variant == "link":
                    add_symlink(output, f"{PACKAGE}/docs/link", "index.md")
                elif variant == "traversal":
                    add_file(output, f"{PACKAGE}/docs/../escape", b"escape\n")
                elif variant == "duplicate":
                    add_file(output, f"{PACKAGE}/README.md", b"duplicate\n")


class InstallerExtractionErrorTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.validator = self.root / "validator.py"
        self.validator.write_text(validator_source(), encoding="utf-8")
        self.driver = self.root / "fault_driver.py"
        self.driver.write_text(FAULT_DRIVER, encoding="utf-8")
        self.archive = self.root / f"{PACKAGE}.tar.gz"
        write_source_archive(self.archive)
        self.digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        self.extractions = 0

    def run_validator(self, archive=None, digest=None, injected=None):
        """Run the real validator; returns (result, extraction directory)."""
        self.extractions += 1
        extraction = self.root / f"extracted-{self.extractions}"
        extraction.mkdir()
        archive = self.archive if archive is None else archive
        arguments = [
            str(archive),
            str(extraction),
            PACKAGE,
            "source",
            self.digest if digest is None else digest,
            "",
        ]
        environment = {**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}
        if injected is None:
            command = [sys.executable, str(self.validator), *arguments]
        else:
            number, suffix = injected
            environment |= {
                "OCTET_TEST_FAULT_ERRNO": str(number),
                "OCTET_TEST_FAULT_SUFFIX": suffix,
            }
            command = [sys.executable, str(self.driver), str(self.validator), *arguments]
        return subprocess.run(
            command,
            capture_output=True,
            text=True,
            env=environment,
            timeout=120,
        ), extraction

    def assert_filesystem_diagnostic(self, result, extraction, number, suffix):
        self.assertEqual(result.returncode, 1, result.stderr)
        message = result.stderr
        name = errno.errorcode[number]
        # Class, errno symbol/number/strerror, destination and one actionable hint.
        self.assertIn(f"OSError {name} ({number}): {os.strerror(number)}", message)
        self.assertIn(str(extraction), message)
        self.assertIn("local filesystem failure", message)
        self.assertIn("TMPDIR", message)
        self.assertIn("re-run the installer", message)
        # A refused write is not an archive verdict, and the diagnostic is not a crash.
        self.assertNotIn(MASKED_MESSAGE, message)
        self.assertNotIn("Traceback", message)
        self.assertEqual(len(message.splitlines()), 1, message)
        # No integrity or installation claim is made about the archive itself.
        self.assertNotIn("intact", message)
        self.assertNotIn("nothing was installed", message)

    def test_disk_quota_during_extraction_names_the_quota(self):
        result, extraction = self.run_validator(injected=(errno.EDQUOT, "docs/index.md"))
        self.assert_filesystem_diagnostic(result, extraction, errno.EDQUOT, "docs/index.md")
        self.assertIn("disk quota", result.stderr)
        self.assertIn("HOME-backed TMPDIR", result.stderr)

    def test_no_space_during_extraction_names_the_full_filesystem(self):
        result, extraction = self.run_validator(injected=(errno.ENOSPC, "examples/README.md"))
        self.assert_filesystem_diagnostic(result, extraction, errno.ENOSPC, "examples/README.md")
        self.assertIn("free space", result.stderr)

    def test_other_filesystem_failure_keeps_permission_hint(self):
        result, extraction = self.run_validator(injected=(errno.EACCES, "sdk/README.md"))
        self.assert_filesystem_diagnostic(result, extraction, errno.EACCES, "sdk/README.md")
        self.assertIn("write permissions", result.stderr)
        self.assertNotIn("HOME-backed TMPDIR", result.stderr)

    def test_refused_write_stops_extraction_and_installs_nothing_here(self):
        result, extraction = self.run_validator(injected=(errno.EDQUOT, "docs/index.md"))
        self.assertEqual(result.returncode, 1)
        # Extraction stops at the refused member: no file was created for it.
        self.assertFalse((extraction / PACKAGE / "docs" / "index.md").exists())
        self.assertEqual(result.stdout, "")

    def test_integrity_rejections_stay_malformed_archive_errors(self):
        cases = {}
        for variant, expected in (
            ("link", "links or unexpected entry types"),
            ("traversal", "unsafe or non-portable path"),
            ("duplicate", "duplicate member"),
        ):
            archive = self.root / f"{variant}.tar.gz"
            write_source_archive(archive, variant)
            cases[variant] = (archive, expected)
        truncated = self.root / "truncated.tar.gz"
        truncated.write_bytes(self.archive.read_bytes()[: self.archive.stat().st_size // 2])
        cases["truncated"] = (truncated, "invalid gzip data")
        garbage = self.root / "garbage.tar.gz"
        garbage.write_bytes(b"not a gzip stream\n")
        cases["garbage"] = (garbage, "invalid gzip data")
        for variant, (archive, expected) in cases.items():
            with self.subTest(variant=variant):
                digest = hashlib.sha256(archive.read_bytes()).hexdigest()
                result, _ = self.run_validator(archive=archive, digest=digest)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(expected, result.stderr)
                self.assertNotIn("local filesystem failure", result.stderr)

    def test_tampered_archive_fails_checksum_before_extraction(self):
        result, extraction = self.run_validator(digest="0" * 64,
                                                injected=(errno.EDQUOT, "docs/index.md"))
        self.assertEqual(result.returncode, 1)
        self.assertIn("checksum mismatch for release archive", result.stderr)
        self.assertNotIn("EDQUOT", result.stderr)
        self.assertFalse((extraction / PACKAGE).exists())


if __name__ == "__main__":
    unittest.main()

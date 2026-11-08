"""Exact-source RC packaging: no binaries/factories are executed by these fixtures."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
LINUX = "x86_64-unknown-linux-gnu"
WINDOWS = "x86_64-pc-windows-gnu"
WINDOWS_MSVC = "x86_64-pc-windows-msvc"
DARWIN_X64 = "x86_64-apple-darwin"
DARWIN_ARM64 = "aarch64-apple-darwin"
IDS = ["octet-codemode", "octet-computer-use"]
ELF_X86_64, ELF_AARCH64 = 62, 183
MACHO_X86_64, MACHO_ARM64 = 0x01000007, 0x0100000C
PE_X86_64, PE_I386 = 0x8664, 0x014C


def elf_payload(machine=ELF_X86_64, e_type=2):
    """A 64-byte little-endian ELF64 executable header; never executed here."""
    header = bytearray(64)
    header[0:4] = b"\x7fELF"
    header[4] = 2   # ELFCLASS64
    header[5] = 1   # ELFDATA2LSB
    header[6] = 1   # EV_CURRENT
    header[16:18] = e_type.to_bytes(2, "little")
    header[18:20] = machine.to_bytes(2, "little")
    return bytes(header) + b"fixture native Linux payload; never executed"


def macho_payload(cpu_type=MACHO_ARM64, file_type=2):
    header = bytearray(32)
    header[0:4] = b"\xcf\xfa\xed\xfe"  # MH_MAGIC_64, little endian
    header[4:8] = cpu_type.to_bytes(4, "little")
    header[12:16] = file_type.to_bytes(4, "little")  # MH_EXECUTE
    return bytes(header) + b"fixture native Mach-O payload; never executed"


def pe_payload(machine=PE_X86_64, optional_magic=0x020B):
    optional = bytearray(0xF0)
    optional[0:2] = optional_magic.to_bytes(2, "little")
    dos = bytearray(0x80)
    dos[0:2] = b"MZ"
    dos[0x3C:0x40] = (0x80).to_bytes(4, "little")
    coff = (b"PE\0\0" + machine.to_bytes(2, "little") + b"\0" * 14
            + len(optional).to_bytes(2, "little") + b"\0" * 2)
    return bytes(dos) + coff + bytes(optional) + b"fixture native Windows payload; never executed"


def mz_without_pe_payload():
    dos = bytearray(0x80)
    dos[0:2] = b"MZ"
    dos[0x3C:0x40] = (0x40).to_bytes(4, "little")
    return bytes(dos)


class RcExtensionBundleTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source"
        scripts = self.source / "scripts"
        scripts.mkdir(parents=True)
        for name in ("package-octet-extension-release.sh", "package-octet-rc-extensions.sh"):
            shutil.copy2(ROOT / "scripts" / name, scripts / name)
        for extension in IDS:
            directory = self.source / "extensions" / extension
            (directory / "bin").mkdir(parents=True)
            (directory / "extension.toml").write_text(
                f'name = "{extension}"\nversion = "0.9.6"\napi_version = "0.4"\n'
                'requires_octet = "=0.9.6"\n[entrypoint]\ncommand = "bin/codemode"\nargs = ["serve"]\n',
                encoding="utf-8",
            )
            entry = directory / "bin" / "codemode"
            entry.write_text("#!/bin/sh\necho must-not-execute >&2\nexit 99\n")
            entry.chmod(0o755)
        (self.source / "extensions" / "release-catalog.txt").write_text("\n".join(IDS) + "\n")
        self.git("init", "--quiet")
        self.git("config", "user.name", "RC fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("add", "scripts", "extensions")
        self.git("commit", "--quiet", "-m", "fixture")
        self.native(LINUX).parent.mkdir(parents=True)
        self.native(LINUX).write_bytes(elf_payload())
        self.native(WINDOWS).parent.mkdir(parents=True)
        self.native(WINDOWS).write_bytes(pe_payload())
        for target, cpu_type in ((DARWIN_X64, MACHO_X86_64), (DARWIN_ARM64, MACHO_ARM64)):
            self.native(target).parent.mkdir(parents=True)
            self.native(target).write_bytes(macho_payload(cpu_type))

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.source), *args], text=True).strip()

    @staticmethod
    def suffix(target):
        return ".exe" if "-windows-" in target else ""

    def native(self, target):
        return (self.source / "extensions/octet-codemode/target" / target / "release"
                / f"octet-codemode{self.suffix(target)}")

    def package(self, target, output="output"):
        return subprocess.run(
            ["bash", str(self.source / "scripts/package-octet-rc-extensions.sh"),
             target, str(self.root / output), "v0.9.6", str(self.source)],
            env={**os.environ, "SOURCE_DATE_EPOCH": "1700000000"},
            capture_output=True, text=True, timeout=30,
        )

    def package_catalog(self, target, output="catalog"):
        """The release-serve.yml invocation: no source override, explicit target."""
        return subprocess.run(
            ["bash", str(self.source / "scripts/package-octet-extension-release.sh"),
             "octet-codemode", str(self.root / output), "v0.9.6", "", target],
            env={**os.environ, "SOURCE_DATE_EPOCH": "1700000000"},
            capture_output=True, text=True, timeout=30,
        )

    def package_source(self, target, output="direct"):
        return subprocess.run(
            ["bash", str(self.source / "scripts/package-octet-extension-release.sh"),
             "octet-codemode", str(self.root / output), "v0.9.6",
             str(self.source / "extensions/octet-codemode"), target],
            env={**os.environ, "SOURCE_DATE_EPOCH": "1700000000"},
            capture_output=True, text=True, timeout=30,
        )

    def test_catalog_closure_native_payload_checksums_and_repeatability(self):
        # A workstation cache must never appear in an immutable-source bundle.
        (self.source / "extensions/octet-computer-use/local-canary").write_text("untracked")
        for output in ("first", "second"):
            result = self.package(LINUX, output)
            self.assertEqual(result.returncode, 0, result.stderr)
        first, second = self.root / "first", self.root / "second"
        self.assertEqual(sorted(path.name for path in first.glob("*.tar.gz")),
                         [f"octet-codemode-0.9.6-{LINUX}.tar.gz", "octet-computer-use-0.9.6.tar.gz"])
        for path in first.iterdir():
            self.assertEqual(path.read_bytes(), (second / path.name).read_bytes())
        info = json.loads((first / "BUILD-INFO.json").read_text())
        self.assertEqual(info["source_commit"], self.git("rev-parse", "HEAD"))
        self.assertEqual(info["source_tree"], self.git("rev-parse", "HEAD^{tree}"))
        self.assertEqual(info["target"], LINUX)
        self.assertEqual(info["extensions"], IDS)
        self.assertTrue(info["factory_enablement"].startswith("none;"))
        for line in (first / "SHA256SUMS").read_text().splitlines():
            digest, name = line.split("  ")
            self.assertEqual(digest, hashlib.sha256((first / name).read_bytes()).hexdigest())
        with tarfile.open(first / f"octet-codemode-0.9.6-{LINUX}.tar.gz") as archive:
            executable = archive.getmember("octet-codemode/bin/codemode")
            self.assertEqual(executable.mode, 0o755)
            self.assertEqual(archive.extractfile(executable).read(), self.native(LINUX).read_bytes())
        with tarfile.open(first / "octet-computer-use-0.9.6.tar.gz") as archive:
            self.assertNotIn("octet-computer-use/local-canary", archive.getnames())
        self.assertEqual(self.git("diff", "--name-only", "HEAD"), "")

    def test_windows_bundle_uses_native_exe_and_retains_serve_argument(self):
        result = self.package(WINDOWS)
        self.assertEqual(result.returncode, 0, result.stderr)
        with tarfile.open(self.root / f"output/octet-codemode-0.9.6-{WINDOWS}.tar.gz") as archive:
            self.assertNotIn("octet-codemode/bin/codemode", archive.getnames())
            self.assertEqual(archive.extractfile("octet-codemode/bin/codemode.exe").read(),
                             self.native(WINDOWS).read_bytes())
            manifest = tomllib.loads(archive.extractfile("octet-codemode/extension.toml").read().decode())
            self.assertEqual(manifest["entrypoint"], {"command": "bin/codemode.exe", "args": ["serve"]})
            self.assertEqual(manifest["requires_octet"], "=0.9.6")
        # Platform staging must not change source, even for Windows.
        self.assertEqual(self.git("diff", "--name-only", "HEAD"), "")

    def test_each_supported_target_ships_only_its_own_payload(self):
        for target in (LINUX, DARWIN_X64, DARWIN_ARM64, WINDOWS):
            with self.subTest(target=target):
                result = self.package(target, f"only-{target}")
                self.assertEqual(result.returncode, 0, result.stderr)
                suffix = self.suffix(target)
                archive = self.root / f"only-{target}/octet-codemode-0.9.6-{target}.tar.gz"
                self.assertTrue(archive.is_file(), archive)
                with tarfile.open(archive) as bundle:
                    self.assertEqual(
                        bundle.extractfile(f"octet-codemode/bin/codemode{suffix}").read(),
                        self.native(target).read_bytes(),
                    )
                    manifest = tomllib.loads(
                        bundle.extractfile("octet-codemode/extension.toml").read().decode())
                    self.assertEqual(manifest["entrypoint"],
                                     {"command": f"bin/codemode{suffix}", "args": ["serve"]})
                    self.assertFalse(any("/target/" in name for name in bundle.getnames()))

    def test_msvc_payload_packages_exe_entrypoint(self):
        native = self.native(WINDOWS_MSVC)
        native.parent.mkdir(parents=True)
        native.write_bytes(pe_payload())
        result = self.package_source(WINDOWS_MSVC, "msvc")
        self.assertEqual(result.returncode, 0, result.stderr)
        with tarfile.open(self.root / f"msvc/octet-codemode-0.9.6-{WINDOWS_MSVC}.tar.gz") as bundle:
            self.assertNotIn("octet-codemode/bin/codemode", bundle.getnames())
            self.assertEqual(bundle.extractfile("octet-codemode/bin/codemode.exe").read(),
                             native.read_bytes())
            manifest = tomllib.loads(bundle.extractfile("octet-codemode/extension.toml").read().decode())
            self.assertEqual(manifest["entrypoint"], {"command": "bin/codemode.exe", "args": ["serve"]})

    def test_packager_rejects_mismatched_container_and_machine(self):
        def mutated(payload, index, value):
            data = bytearray(payload)
            data[index] = value
            return bytes(data)

        cases = (
            (LINUX, macho_payload(MACHO_X86_64), f"does not match {LINUX}"),
            (LINUX, elf_payload(ELF_AARCH64), f"does not match {LINUX}"),
            (DARWIN_X64, macho_payload(MACHO_ARM64), f"does not match {DARWIN_X64}"),
            (DARWIN_ARM64, macho_payload(MACHO_X86_64), f"does not match {DARWIN_ARM64}"),
            (DARWIN_ARM64, elf_payload(), f"does not match {DARWIN_ARM64}"),
            (WINDOWS, pe_payload(PE_I386), f"does not match {WINDOWS}"),
            (WINDOWS, elf_payload(), f"does not match {WINDOWS}"),
            (WINDOWS_MSVC, pe_payload(PE_I386), f"does not match {WINDOWS_MSVC}"),
            (WINDOWS_MSVC, mz_without_pe_payload(), "has no PE header"),
            (WINDOWS_MSVC, b"MZ", "truncated DOS header"),
            (WINDOWS_MSVC, pe_payload(optional_magic=0x010B), "not a PE32+ payload"),
            (LINUX, b"\x7fELF", "truncated ELF header"),
            (LINUX, b"\x7fELF" + b"\0" * 16, "truncated ELF header"),
            (LINUX, mutated(elf_payload(), 4, 1), "not a 64-bit little-endian ELF payload"),
            (LINUX, mutated(elf_payload(), 5, 2), "not a 64-bit little-endian ELF payload"),
            (LINUX, elf_payload(e_type=1), "not an executable ELF payload"),
            (DARWIN_ARM64, b"\xcf\xfa\xed\xfe", "truncated Mach-O header"),
            (DARWIN_ARM64, macho_payload(file_type=1), "not an executable Mach-O payload"),
            (DARWIN_ARM64, b"\xca\xfe\xba\xbe" + b"\0" * 12, "universal Mach-O payload"),
            (LINUX, b"plain text, not a native payload", "wrong target format"),
        )
        for target, payload, message in cases:
            with self.subTest(target=target, message=message):
                native = self.native(target)
                native.parent.mkdir(parents=True, exist_ok=True)
                native.write_bytes(payload)
                result = self.package_source(target, "invalid")
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn(message, result.stderr)
                self.assertEqual(list((self.root / "invalid").glob("*.tar.gz")), [])

    def test_catalog_packaging_replaces_the_tracked_launcher_with_the_native_payload(self):
        # release-serve.yml packages without a source override: the tracked
        # launcher must be replaced by the staged native runner, never both.
        result = self.package_catalog(LINUX)
        self.assertEqual(result.returncode, 0, result.stderr)
        archive = self.root / f"catalog/octet-codemode-0.9.6-{LINUX}.tar.gz"
        with tarfile.open(archive) as bundle:
            names = bundle.getnames()
            self.assertEqual(bundle.extractfile("octet-codemode/bin/codemode").read(),
                             self.native(LINUX).read_bytes())
            self.assertFalse(any("/target/" in name for name in names))
            manifest = tomllib.loads(bundle.extractfile("octet-codemode/extension.toml").read().decode())
            self.assertEqual(manifest["entrypoint"], {"command": "bin/codemode", "args": ["serve"]})
        self.assertEqual(self.git("diff", "--name-only", "HEAD"), "")

    def test_dirty_source_fails_before_packaging(self):
        (self.source / "extensions/release-catalog.txt").write_text("octet-codemode\n")
        result = self.package(LINUX)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("tracked changes", result.stderr)
        self.assertFalse((self.root / "output").exists())

    def test_missing_or_linked_native_executable_fails_closed(self):
        native = self.native(LINUX)
        native.unlink()
        for linked in (False, True):
            if linked:
                native.symlink_to(self.native(WINDOWS))
            result = self.package(LINUX)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("missing regular native", result.stderr)
            self.assertFalse((self.root / "output").exists())

    def test_unsupported_target_fails_before_packaging(self):
        result = self.package("arm-unknown-unqualified")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported dogfood target", result.stderr)
        self.assertFalse((self.root / "output").exists())

    def test_native_codemode_is_covered_by_the_targeted_release_paths(self):
        # The universal catalog loop cannot package codemode: it needs a staged
        # native runner and an explicit target. Coverage moves to the native
        # release-candidate matrix and release-serve's build-codemode job, and
        # those call sites are asserted here instead of being dropped.
        ci = (ROOT / ".github/workflows/ci.yml").read_text()
        quality = ci.split("\n  quality:\n", 1)[1].split("\n  first-party-extension-tests:\n", 1)[0]
        self.assertIn('"$extension" != octet-codemode', quality)
        self.assertIn("scripts/package-octet-extension-release.sh", quality)
        candidate = ci.split("\n  release-candidate:\n", 1)[1].split("\n  msrv:\n", 1)[0]
        self.assertIn("extensions/octet-codemode/Cargo.toml", candidate)
        self.assertIn("octet-codemode-$version-${{ matrix.target }}.tar.gz", candidate)
        windows = ci.split("\n  windows:\n", 1)[1].split("\n  release-candidate:\n", 1)[0]
        self.assertIn("extensions/octet-codemode/Cargo.toml", windows)
        serve = (ROOT / ".github/workflows/release-serve.yml").read_text()
        job = serve.split("\n  build-codemode:\n", 1)[1].split("\n  publish:\n", 1)[0]
        for target in (LINUX, DARWIN_X64, DARWIN_ARM64, WINDOWS, WINDOWS_MSVC):
            self.assertIn(target, job)
        self.assertIn(
            'octet-codemode "$RUNNER_TEMP/octet-codemode" '
            '"${{ needs.resolve.outputs.tag }}" "" "${{ matrix.target }}"',
            job,
        )
        self.assertIn(
            "CODEMODE_RUNNER: ${{ github.workspace }}/extensions/octet-codemode/target/"
            "${{ matrix.target }}/release/octet-codemode${{ matrix.suffix }}",
            job,
        )
        self.assertIn("-p test_bundle.py", job)
        self.assertIn("pattern: octet-codemode-*", serve.split("\n  publish:\n", 1)[1])

    def test_workflow_dispatch_is_unsigned_and_includes_each_platform_bundle(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        job = workflow.split("\n  release-candidate:\n", 1)[1].split("\n  msrv:\n", 1)[0]
        self.assertIn("if: github.event_name == 'workflow_dispatch'", job)
        for target in (LINUX, "x86_64-apple-darwin", "aarch64-apple-darwin"):
            self.assertIn(target, job)
        self.assertIn("package-octet-rc-extensions.sh", job)
        self.assertIn("octet-rc/", job)
        self.assertIn("-c SHA256SUMS", job)
        self.assertNotIn("contents: write", workflow)
        self.assertNotIn("gh release create", workflow)
        windows = workflow.split("\n  windows:\n", 1)[1].split("\n  release-candidate:\n", 1)[0]
        self.assertIn("package-octet-rc-extensions.sh", windows)
        self.assertIn("'octet-rc'", windows)
        self.assertIn("'workflow_dispatch' || success()", windows)


if __name__ == "__main__":
    unittest.main()

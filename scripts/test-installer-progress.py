#!/usr/bin/env python3
"""Offline UI regressions, invoked by test-binary-installer.sh with its fixtures.

Full installations use fake commands and signed-package fixtures. Measured bar
checks use the real curl against 127.0.0.1 only; HTTPS/trust are changed only in
an extracted test helper, never in the published installer. No Rust build runs.
"""

import errno
import fcntl
import http.server
import os
from pathlib import Path
import re
import select
import shlex
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import unittest

INSTALLER = Path(sys.argv.pop(1))
FIXTURES = Path(sys.argv.pop(1))
PREFIX = INSTALLER.read_text().split("sha256_file() {", 1)[0]
VERSION = re.search(r'^version="([^"]+)"', PREFIX, re.MULTILINE)[1]


def capture(command, env, columns=None, hook=None):
    if columns is None:
        result = subprocess.run(command, env=env, stdin=subprocess.DEVNULL,
                                capture_output=True, timeout=20)
        return result.returncode, result.stdout, result.stderr
    master, slave = os.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, columns, 0, 0))
    attributes = termios.tcgetattr(slave)
    # Give curl a real controlling terminal, as in a user shell/SSH session.
    # A stderr PTY alone cannot exercise curl's terminal-width detection.
    command = [sys.executable, "-c",
               "import fcntl,os,sys,termios; fcntl.ioctl(2,termios.TIOCSCTTY,0); "
               "os.execvpe(sys.argv[1],sys.argv[1:],os.environ)", *command]
    process = subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=slave, start_new_session=True)
    output, errors = bytearray(), bytearray()
    descriptors = [master, process.stdout.fileno()]
    deadline = time.monotonic() + 20
    try:
        while True:
            ready, _, _ = select.select(descriptors, [], [], 0.05)
            chunk = b""
            for descriptor in ready:
                try:
                    data = os.read(descriptor, 65536)
                except OSError as error:
                    if error.errno not in (errno.EIO, errno.ENXIO):
                        raise
                    data = b""
                if not data:
                    descriptors.remove(descriptor)
                elif descriptor == master:
                    errors.extend(data)
                    chunk += data
                else:
                    output.extend(data)
            try:
                current_attributes = termios.tcgetattr(slave)
            except termios.error as error:
                # macOS revokes the controlling PTY when the session exits.
                if error.args[0] != errno.ENOTTY:
                    raise
            else:
                assert current_attributes == attributes, "installer changed terminal modes"
            if hook:
                hook(process, chunk, bytes(errors))
            if process.poll() is not None and not ready:
                break
            if time.monotonic() > deadline:
                raise AssertionError(f"installer timed out: {errors!r}")
        return process.returncode, bytes(output), bytes(errors).replace(b"\r\n", b"\n")
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        process.stdout.close()
        os.close(master)
        os.close(slave)


class InstallerProgressTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="progress-", dir=FIXTURES)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "tmp").mkdir()
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith("OCTET_") or key.startswith("OCTET_TEST_")}
        self.env.update(HOME=str(self.root), SHELL="/bin/sh", TERM="xterm-256color",
                        LC_ALL="C", LANG="C", COLUMNS="999", TMPDIR=str(self.root / "tmp"),
                        OCTET_INSTALL_DIR=str(self.root / "bin"), OCTET_NO_MODIFY_PATH="1",
                        OCTET_TEST_CURL_LOG=str(self.root / "curl.log"), NO_PROXY="*", no_proxy="*")
        for key in list(self.env):
            if key.lower() in {"http_proxy", "https_proxy", "all_proxy"}:
                del self.env[key]

    def install(self, columns=None, args=()):
        result = capture(["sh", str(INSTALLER), *args], self.env, columns)
        self.assertEqual(list((self.root / "tmp").iterdir()), [], "temporary files leaked")
        self.assertNotIn(b"\x1b", result[2], "installer emitted ANSI/colour/cursor controls")
        return result

    def assert_success(self, result, animated, columns=80):
        status, stdout, stderr = result
        self.assertEqual(status, 0, stderr.decode())
        self.assertEqual(stdout, b"", "presentation contaminated machine-readable stdout")
        self.assertIn(f"octet v{VERSION} is installed.".encode(), stderr)
        stages = [b"Downloading release checksums", b"Downloading release signature",
                  b"Downloading signature verifier", b"Verifying signature verifier checksum",
                  b"Verifying release signature and checksums", b"Downloading octet",
                  b"Verifying checksum, validating and extracting archive",
                  b"Checking executable version and host handshake", b"Installing executables",
                  b"Installing documentation", b"Checking installed version", b"is installed."]
        offsets = [stderr.index(stage) for stage in stages]
        self.assertEqual(offsets, sorted(offsets))
        if not animated:
            self.assertNotIn(b"\r", stderr)
            self.assertNotIn(b"%", stderr)
        calls = (self.root / "curl.log").read_text().splitlines()
        self.assertEqual(len(calls), 4)
        for call in calls:
            args = shlex.split(call)
            expected_columns = min(columns - 1, 79) if animated else 79
            self.assertEqual(args[0], f"COLUMNS={expected_columns}")
            for flag, value in {"--proto": "=https", "--proto-redir": "=https",
                                "--max-redirs": "5", "--retry": "3", "--retry-delay": "1",
                                "--connect-timeout": "15", "--max-time": "300",
                                "--write-out": "%{url_effective}"}.items():
                self.assertEqual(args[args.index(flag) + 1], value)
            for flag in ["--tlsv1.2", "--location", "--fail", "--show-error",
                         "--dump-header", "--output"]:
                self.assertIn(flag, args)
            self.assertEqual("--progress-bar" in args, animated)
            self.assertEqual("--silent" in args, not animated)

    def test_redirected_is_plain_and_stderr_only(self):
        result = self.install()
        self.assert_success(result, False)
        self.assertTrue(result[2].startswith(f"octet / v{VERSION} / Installing\n".encode()))

    def test_dumb_and_empty_term_ttys_are_plain(self):
        for term in ("dumb", ""):
            with self.subTest(term=term):
                self.env["TERM"] = term
                (self.root / "curl.log").unlink(missing_ok=True)
                self.assert_success(self.install(80), False)

    def test_utf8_canonical_monochrome_heading(self):
        self.env["LC_ALL"] = "en_US.UTF-8"
        result = self.install(80)
        self.assert_success(result, True)
        self.assertIn((f"\n    ████  ████████   octet\n    ████  ████████   v{VERSION}\n"
                       "  ████████████████   Installing\n  ████████████████\n").encode(), result[2])
        (self.root / "curl.log").unlink()
        result = self.install(12)
        self.assert_success(result, False)
        self.assertIn("\n   ██ ████\n  ████████\noctet\n".encode(), result[2])

    def test_ascii_and_narrow_heading(self):
        for width in (80, 31, 30, 18, 17, 10, 9, 5):
            with self.subTest(width=width):
                (self.root / "curl.log").unlink(missing_ok=True)
                result = self.install(width)
                self.assert_success(result, width >= 22, width)
                heading = result[2].split(b"Downloading", 1)[0]
                self.assertNotIn("█".encode(), heading)
                self.assertTrue(all(len(line) <= width for line in heading.splitlines()))
                if width >= 18:
                    self.assertEqual(heading.count(b"    ####  ########"), 2)
                    self.assertEqual(heading.count(b"  ################"), 2)
                elif width >= 10:
                    self.assertIn(b"\n   ## ####\n  ########\n", heading)
                else:
                    self.assertNotIn(b"#", heading)
                self.assertIn(f"v{VERSION}".encode(), heading.replace(b"\n", b""))
                self.assertEqual(b"   octet\n" in heading, width >= 31)

    def test_header_wraps_complete_long_version(self):
        version = "v123.456.789+long-build-metadata"
        helper = self.root / "long-version.sh"
        helper.write_text(PREFIX + f"\ntag={shlex.quote(version)}\nui_heading\n")
        for width in (31, 12, 5):
            with self.subTest(width=width):
                status, stdout, stderr = capture(["sh", str(helper)], self.env, width)
                self.assertEqual(status, 0)
                self.assertEqual(stdout, b"")
                self.assertTrue(all(len(line) <= width for line in stderr.splitlines()))
                self.assertIn(version.encode(), stderr.replace(b"\n", b""))
                self.assertNotIn(b"   octet\n", stderr)

    def test_parent_handoff_skips_only_heading(self):
        self.env["OCTET_UPDATE_PARENT_UI"] = "1"
        result = self.install(80)
        self.assert_success(result, True)
        self.assertTrue(result[2].startswith(b"  Downloading release checksums"))
        self.assertNotIn(b"  octet\n", result[2])

    def test_download_signature_and_final_probe_fail_without_completion(self):
        for variable, status, message in [
            ("OCTET_TEST_DOWNLOAD_FAIL", 1, b"fixture download failed"),
            ("OCTET_TEST_BAD_SIGNATURE", 1, b"release checksum provenance verification failed"),
            ("OCTET_TEST_FINAL_VERSION_FAIL", 42, b"could not verify the installed octet version"),
            ("OCTET_TEST_FINAL_VERSION_WRONG", 1, b"installed octet binary version mismatch"),
        ]:
            with self.subTest(variable=variable):
                self.env[variable] = "1"
                result = self.install(80)
                self.assertEqual(result[0], status, result[2].decode())
                self.assertEqual(result[1], b"")
                self.assertIn(message, result[2])
                self.assertIn(b"octet installation failed", result[2])
                self.assertNotIn(b"is installed.", result[2])
                self.assertNotIn(b"installed version probe failed", result[2])
                self.assertNotIn(b"wrong installed binary output", result[2])
                self.assertNotIn(b"unexpected installed binary diagnostic", result[2])
                del self.env[variable]

    def test_build_failure_keeps_child_diagnostics_without_animation(self):
        result = self.install(80, ["--from-source"])
        self.assertEqual(result[0], 99)
        self.assertIn(b"Building octet", result[2])
        self.assertIn(b"binary installer unexpectedly invoked Cargo", result[2])
        self.assertNotIn(b"%", result[2])
        self.assertNotIn(b"\r", result[2])
        self.assertNotIn(b"is installed.", result[2])

    def test_interrupt_cleans_up_and_preserves_exit_status(self):
        helper = self.root / "interrupt.sh"
        helper.write_text(PREFIX + '\nui_heading\nui_stage "Waiting"\nsleep 30\nexit 0\n')
        for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            sent = False
            waiting_since = None

            def interrupt(process, chunk, errors):
                nonlocal sent, waiting_since
                if waiting_since is None and b"Waiting" in errors:
                    waiting_since = time.monotonic()
                # Let the foreground child start, just as a user interrupting
                # active work would; do not race the shell's fork/exec window.
                if not sent and waiting_since is not None and time.monotonic() - waiting_since > 0.2:
                    sent = True
                    os.killpg(process.pid, sig)

            result = capture(["sh", str(helper)], self.env, 80, interrupt)
            self.assertEqual(result[0], 128 + sig, result[2].decode())
            self.assertIn(b"octet installation failed", result[2])
            self.assertNotIn(b"is installed.", result[2])
            self.assertEqual(list((self.root / "tmp").iterdir()), [])

    def test_real_curl_known_and_unknown_length_progress(self):
        for known_size in (True, False):
            with self.subTest(known_size=known_size):
                self.real_transfer(known_size)

    def test_real_curl_narrow_fallback_and_bounded_bars(self):
        # curl 8.7.1 ignores COLUMNS <= 20, drawing a 79-cell fallback;
        # with >20 it honors COLUMNS, including unknown-length activity.
        # The installer reserves one column, and overrides inherited COLUMNS=999.
        for columns, known_size in ((20, True), (21, False), (22, True),
                                    (22, False), (40, True), (79, False), (120, True)):
            with self.subTest(columns=columns, known_size=known_size):
                self.real_transfer(known_size, columns)

    def real_transfer(self, known_size, columns=80):
        total = 64 * 1024
        sent = 0
        observations = []

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                nonlocal sent
                self.send_response(200)
                if known_size:
                    self.send_header("Content-Length", str(total))
                self.end_headers()
                for index in range(8):
                    sent += 8192
                    self.wfile.write(b"x" * 8192)
                    self.wfile.flush()
                    time.sleep(0.3 if index != 3 else 0.8)

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            # Test-only HTTP fixture adaptation. The full-install cases above
            # assert the untouched production HTTPS and redirect restrictions.
            prefix = PREFIX.replace("--proto '=https'", "--proto '=http'")
            prefix = prefix.replace("--proto-redir '=https'", "--proto-redir '=http'")
            prefix = prefix.replace('COLUMNS="$curl_columns" curl \\',
                                    'COLUMNS="$curl_columns" ' + shlex.quote(self.env["OCTET_TEST_REAL_CURL"]) + " \\")
            url = f"http://127.0.0.1:{server.server_port}/fixture"
            helper = self.root / "transfer.sh"
            helper.write_text(prefix + f'''
trusted_release_url() {{ [ "$1" = {shlex.quote(url)} ]; }}
ui_stage "Downloading fixture"
download_release_file {shlex.quote(url)} "$work_directory/fixture"
[ "$(wc -c < "$work_directory/fixture" | tr -d '[:space:]')" = {total} ]
ui_stage "Verifying fixture"
''')

            def observe(process, chunk, errors):
                if chunk:
                    observations.append((sent, errors))

            status, stdout, stderr = capture(["sh", str(helper)], self.env, columns, observe)
            self.assertEqual(status, 0, stderr.decode())
            self.assertEqual(stdout, b"", "effective URL escaped capture")
            self.assertIn(b"Verifying fixture", stderr)
            self.assertNotIn(b"\x1b", stderr)
            if columns < 22:
                self.assertNotIn(b"\r", stderr, "narrow fallback must not animate")
                self.assertNotIn(b"%", stderr, "narrow fallback must not invent progress")
            else:
                self.assertTrue(any(count < total and b"\r" in chunk for count, chunk in observations),
                                "no responsive progress before transfer completion")
                for frame in stderr.split(b"\r")[1:]:
                    self.assertLess(len(frame.split(b"\n", 1)[0]), columns,
                                    "native curl meter would wrap on this terminal")
            percentages = []
            for count, chunk in observations:
                for value in re.findall(rb"(\d+\.\d+)%", chunk):
                    percentage = float(value)
                    percentages.append(percentage)
                    self.assertLessEqual(percentage, count / total * 100 + 0.1,
                                         "progress ran ahead of delivered bytes")
            if known_size and columns >= 22:
                self.assertTrue(any(0 < value < 100 for value in percentages), stderr)
                self.assertIn(100.0, percentages)
            else:
                self.assertEqual(percentages, [], "unmeasured phase invented a percent")
            after_transfer = stderr.split(b"Verifying fixture", 1)[1]
            self.assertNotIn(b"\r", after_transfer)
            self.assertEqual(list((self.root / "tmp").iterdir()), [])
        finally:
            server.shutdown()
            server.server_close()
            worker.join(timeout=2)


if __name__ == "__main__":
    unittest.main()

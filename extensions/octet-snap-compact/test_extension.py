"""Process-level API 0.4 handshake and real oxi renderer smoke."""

import base64
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
# The renderer is a separate package pinned to Rust 1.96, above octet's 1.86
# MSRV and above the toolchain CI installs, so this suite skips when the
# available toolchain cannot build it.
RENDERER_RUST_VERSION = (1, 96)


def rust_version() -> tuple[int, int]:
    """The active toolchain's version, or () when there is no usable rustc."""

    for candidate in ([os.environ["CARGO"]] if "CARGO" in os.environ else ["cargo"]):
        try:
            completed = subprocess.run(
                [candidate, "--version"], capture_output=True, text=True, timeout=60
            )
        except (OSError, subprocess.SubprocessError):
            continue
        if completed.returncode != 0:
            continue
        words = completed.stdout.split()
        # Either "cargo 1.97.1 (...)" or "rustc 1.97.1 (...)".
        if len(words) < 2:
            continue
        try:
            major, minor = words[1].split(".")[:2]
            return int(major), int(minor)
        except ValueError:
            continue
    return ()


class SnapcompactProcessTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if shutil.which(os.environ.get("CARGO", "cargo")) is None:
            raise unittest.SkipTest("cargo is unavailable")
        active = rust_version()
        if not active:
            raise unittest.SkipTest("no usable rustc on PATH")
        if active < RENDERER_RUST_VERSION:
            raise unittest.SkipTest(
                f"rustc {active[0]}.{active[1]} cannot build the renderer; "
                f"needs {RENDERER_RUST_VERSION[0]}.{RENDERER_RUST_VERSION[1]}"
            )
        # Build outside the extension directory. A cargo target/ tree here would
        # exceed the host's bounded source walk (1,024 entries), so the host
        # would mark the source unverified and park the extension.
        cls._target = tempfile.TemporaryDirectory(prefix="octet-snap-renderer-")
        completed = subprocess.run(
            [os.environ.get("CARGO", "cargo"), "build", "--release", "--locked",
             "--quiet", "--target-dir", cls._target.name,
             "--manifest-path", str(ROOT / "renderer" / "Cargo.toml")],
            check=False, timeout=900,
        )
        if completed.returncode != 0:
            cls._target.cleanup()
            raise unittest.SkipTest("renderer build failed on this toolchain")
        binary = Path(cls._target.name) / "release" / "octet-snap-renderer"
        destination = ROOT / "renderer" / "target" / "release"
        destination.mkdir(parents=True, exist_ok=True)
        shutil.copy2(binary, destination / "octet-snap-renderer")

    @classmethod
    def tearDownClass(cls):
        if getattr(cls, "_target", None) is not None:
            cls._target.cleanup()

    def test_negotiation_png_and_clean_shutdown(self):
        child = subprocess.Popen([str(ROOT / "extension.py")], cwd=ROOT,
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, text=True)
        try:
            def request(id_, method, params):
                child.stdin.write(json.dumps({"jsonrpc": "2.0", "id": id_,
                                              "method": method, "params": params}) + "\n")
                child.stdin.flush()
                response = json.loads(child.stdout.readline())
                self.assertEqual(response["id"], id_)
                return response

            init = request(1, "initialize", {
                "api_version": "0.4",
                "contributes": {"tools": [], "commands": [],
                                "hooks": ["compaction_strategy"]},
                "protocol": {"version": "0.4",
                             "required_features": ["request_cancellation", "content_parts"],
                             "optional_features": ["compaction_strategy"],
                             "limits": {"max_concurrent_requests": 1}},
            })
            self.assertEqual(init["result"]["protocol"]["version"], "0.4")
            self.assertIn("compaction_strategy", init["result"]["protocol"]["features"])
            result = request(2, "hook/run", {
                "hook": "compaction_strategy",
                "payload": {"model_id": "claude-sonnet", "text": "User: hello\nAssistant: hi"},
                "context": {},
            })["result"]
            frames = result["compaction_frames"]
            self.assertTrue(frames)
            self.assertTrue(base64.b64decode(frames[0]).startswith(b"\x89PNG\r\n\x1a\n"))
            invalid = request(3, "hook/run", {
                "hook": "compaction_strategy", "payload": {"model_id": "claude", "text": ""},
                "context": {},
            })
            self.assertIn("error", invalid)
            self.assertIn("result", request(4, "shutdown", {}))
            child.stdin.close()
            self.assertEqual(child.wait(timeout=5), 0)
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            for stream in (child.stdin, child.stdout, child.stderr):
                if stream and not stream.closed:
                    stream.close()


if __name__ == "__main__":
    unittest.main()

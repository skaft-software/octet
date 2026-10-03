"""Process-level API 0.4 handshake and real oxi renderer smoke."""

import base64
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent
# The renderer is a separate package pinned to Rust 1.96, above octet's 1.88
# MSRV. Source-extension CI explicitly installs Rust 1.96; on a supported
# toolchain, renderer compilation failures must fail this suite.
RENDERER_RUST_VERSION = (1, 96)


def cargo_version() -> tuple[int, int]:
    """The active Cargo toolchain version, or () when Cargo is unusable."""

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
        # The CI shim's cargo --version reports the pinned toolchain version.
        if len(words) < 2:
            continue
        try:
            major, minor = words[1].split(".")[:2]
            return int(major), int(minor)
        except ValueError:
            continue
    return ()


def build_renderer(target_dir: str) -> Path:
    completed = subprocess.run(
        [os.environ.get("CARGO", "cargo"), "build", "--release", "--locked",
         "--quiet", "--target-dir", target_dir,
         "--manifest-path", str(ROOT / "renderer" / "Cargo.toml")],
        check=False, timeout=900,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"renderer build failed with exit status {completed.returncode} "
            "on a supported Rust toolchain"
        )
    return Path(target_dir) / "release" / "octet-snap-renderer"


class SnapcompactProcessTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if shutil.which(os.environ.get("CARGO", "cargo")) is None:
            raise unittest.SkipTest("cargo is unavailable")
        active = cargo_version()
        if not active:
            raise unittest.SkipTest("no usable cargo toolchain on PATH")
        if active < RENDERER_RUST_VERSION:
            raise unittest.SkipTest(
                f"Cargo {active[0]}.{active[1]} is older than the renderer's "
                f"required {RENDERER_RUST_VERSION[0]}.{RENDERER_RUST_VERSION[1]} toolchain"
            )
        # Build outside the extension directory. A cargo target/ tree here would
        # exceed the host's bounded source walk (1,024 entries), so the host
        # would mark the source unverified and park the extension.
        cls._target = tempfile.TemporaryDirectory(prefix="octet-snap-renderer-")
        try:
            binary = build_renderer(cls._target.name)
        except Exception:
            cls._target.cleanup()
            raise
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


class RendererBuildFailureTests(unittest.TestCase):
    @patch.object(subprocess, "run")
    def test_compilation_error_is_failure_not_skip(self, run):
        run.return_value.returncode = 101
        with self.assertRaisesRegex(RuntimeError, "exit status 101"):
            build_renderer("unused-target")
        self.assertEqual(run.call_args.kwargs["check"], False)


if __name__ == "__main__":
    unittest.main()

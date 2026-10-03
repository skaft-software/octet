"""Offline release-catalog adapter: runs the actual Pi/WASM and stdio launcher."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = ROOT.parents[1]


def initialize():
    return {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "api_version": "0.4", "octet_version": "0.8.2",
        "extension": {"name": "octet-codemode", "version": "0.8.2"},
        "flag_values": [], "protocol": {"version": "0.4",
            "required_features": ["request_cancellation", "content_parts"],
            "optional_features": ["tool_composition_v1"],
            "limits": {"max_concurrent_requests": 8}}}}


class CodemodeTests(unittest.TestCase):
    def run_checked(self, command, **kwargs):
        result = subprocess.run(command, capture_output=True, text=True, timeout=90, **kwargs)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def test_vendor_integrity_and_offline_regeneration(self):
        result = self.run_checked([sys.executable, str(ROOT / "vendor/regenerate.py"), "--check"])
        self.assertIn("offline", result.stdout)

    def test_real_wasm_runtime_and_transport(self):
        node = shutil.which("node")
        self.assertIsNotNone(node, "Install Node >=22.19.0; this suite must not silently skip the real VM")
        version = self.run_checked([node, "--version"]).stdout.strip().lstrip("v")
        self.assertGreaterEqual(tuple(map(int, version.split("."))), (22, 19, 0))
        self.run_checked([node, "--test", str(ROOT / "tests/runtime.test.mjs")])

    def test_missing_node_is_actionable(self):
        result = subprocess.run([sys.executable, str(ROOT / "extension.py")],
                                env={**os.environ, "PATH": ""}, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 1)
        self.assertIn("Node.js >=22.19.0", result.stderr)
        self.assertEqual(result.stdout, "")

    def probe_launcher(self, bundle, directory):
        # Ambient preloads and a malicious workspace runtime must not be loaded.
        (directory / "runtime.mjs").write_text('throw new Error("workspace runtime was loaded");')
        environment = {**os.environ, "OCTET_EXTENSION_DIR": str(bundle),
                       "OCTET_EXTENSION_SCRATCH": str(directory / "scratch"),
                       "NODE_OPTIONS": "--not-a-real-node-option", "NODE_PATH": str(directory)}
        process = subprocess.Popen([sys.executable, str(bundle / "extension.py")], cwd=directory,
                                   env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True)
        try:
            for request in [initialize(),
                            {"jsonrpc": "2.0", "id": 2, "method": "command/execute",
                             "params": {"name": "codemode", "arguments": ["status"], "context": {}}},
                            {"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {}}]:
                process.stdin.write(json.dumps(request) + "\n")
                process.stdin.flush()
                response = json.loads(process.stdout.readline())
                self.assertEqual(response["id"], request["id"])
                self.assertIn("result", response, response)
                if request["id"] == 1:
                    self.assertEqual(response["result"]["tools"][0]["name"], "codemode")
            self.assertEqual(process.wait(timeout=10), 0, process.stderr.read())
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=10)
            for stream in (process.stdin, process.stdout, process.stderr):
                stream.close()

    def test_launcher_uses_bundle_not_workspace_or_ambient_preloads(self):
        with tempfile.TemporaryDirectory() as temporary:
            self.probe_launcher(ROOT, Path(temporary))

    def test_release_bundle_is_deterministic_complete_and_launches_offline(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            command = ["bash", str(REPOSITORY / "scripts/package-octet-extension-release.sh"),
                       "octet-codemode", str(directory / "out"), "v0.8.2", str(ROOT)]
            environment = {**os.environ, "SOURCE_DATE_EPOCH": "1700000000"}
            self.run_checked(command, env=environment)
            archive = directory / "out/octet-codemode-0.8.2.tar.gz"
            first = archive.read_bytes()
            self.run_checked(command, env=environment)
            self.assertEqual(archive.read_bytes(), first)
            unpacked = directory / "unpacked"
            with tarfile.open(archive) as bundle:
                names = set(bundle.getnames())
                for required in ("extension.py", "runtime.mjs", "vendor/quickjs-wasi/quickjs.wasm",
                                 "vendor/pi-codemode/LICENSE", "vendor/quickjs-wasi/LICENSE",
                                 "vendor/PROVENANCE.json", "vendor/SHA256SUMS", "vendor/regenerate.py"):
                    self.assertIn(f"octet-codemode/{required}", names)
                self.assertTrue(all(member.isfile() or member.isdir() for member in bundle))
                bundle.extractall(unpacked, filter="data")
            self.probe_launcher(unpacked / "octet-codemode", directory)


if __name__ == "__main__":
    unittest.main()

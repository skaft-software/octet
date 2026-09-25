"""Local process evidence for the thin launcher, not Pi runtime conformance."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest

PACKAGE = Path(__file__).resolve().parents[1]
REPOSITORY = PACKAGE.parents[1]
sys.path.insert(0, str(REPOSITORY / "sdk/python"))
from octet_extension import api_v03 as api

BINARY = Path(os.environ.get("OCTET_PI_IMPORT_TEST_BINARY", REPOSITORY / "target/debug/octet"))


@unittest.skipUnless(os.name == "posix" and BINARY.is_file(), "requires a local built octet and POSIX")
class PiAdapterPackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.home = self.root / "home"
        self.home.mkdir()
        self.source = self.root / "pi"
        (self.source / "skills/review").mkdir(parents=True)
        self.settings = json.dumps({
            "model": "openai/gpt-4o-mini",
            "mcpServers": {"docs": {
                "command": "never-run-mcp", "args": ["--stdio"],
                "env": {"TOKEN": "PI_SECRET_ENV"},
                "headers": {"Authorization": "PI_SECRET_HEADER"},
                "cwd": "/private/source",
            }},
            "permissions": {"allow": ["bash"]},
        }).encode()
        (self.source / "settings.json").write_bytes(self.settings)
        (self.source / "skills/review/SKILL.md").write_text("Review before applying.\n")
        (self.source / "auth.json").write_text('{"token":"PI_SECRET_AUTH"}')
        self.before = {str(path.relative_to(self.source)): path.read_bytes()
                       for path in self.source.rglob("*") if path.is_file()}
        self.manifest = tomllib.loads((PACKAGE / "extension.toml").read_text())
        # Mirror the host's script-only staging; no sibling implementation needed.
        self.launcher = self.root / self.manifest["entrypoint"]["command"]
        shutil.copy2(PACKAGE / self.manifest["entrypoint"]["command"], self.launcher)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "octet").symlink_to(BINARY.resolve())
        self.offer = api.host_offer(api.MAX_FRAME_BYTES, 1)

    def initialize(self):
        return ("initialize", {
            "api_version": "0.3", "octet_version": "0.8.1-rc.1",
            "extension": {"name": self.manifest["name"], "version": self.manifest["version"]},
            "workspace": str(self.root), "capabilities": self.manifest["capabilities"],
            "contributes": self.manifest["contributes"], "flag_values": [],
            "host": {}, "contract": self.offer.to_wire(),
        })

    def exchange(self, requests):
        frames = [api.canonical_frame({"jsonrpc": "2.0", "id": index,
                                      "method": method, "params": params}, api.MAX_FRAME_BYTES)
                  for index, (method, params) in enumerate(requests, 1)]
        result = subprocess.run(
            [str(self.launcher)], input=b"\n".join(frames) + b"\n", capture_output=True,
            cwd=self.root, timeout=10,
            env={"HOME": str(self.home), "PATH": f"{self.bin}:/usr/bin:/bin", "LANG": "C.UTF-8"},
        )
        self.assertEqual(0, result.returncode, result.stderr.decode())
        self.assertEqual(b"", result.stderr)
        replies = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual(len(requests), len(replies))
        for index, (line, reply) in enumerate(zip(result.stdout.splitlines(), replies), 1):
            self.assertEqual(line, api.canonical_frame(reply, api.MAX_FRAME_BYTES))
            api.parse_json_rpc_envelope(reply)
            self.assertEqual(index, reply["id"])
        self.assertEqual(self.before, {str(path.relative_to(self.source)): path.read_bytes()
                                     for path in self.source.rglob("*") if path.is_file()})
        self.assertEqual([], list(self.home.iterdir()))
        for secret in (b"PI_SECRET_ENV", b"PI_SECRET_HEADER", b"PI_SECRET_AUTH"):
            self.assertNotIn(secret, result.stdout)
        return replies

    def test_manifest_and_typed_read_only_round_trip(self):
        self.assertEqual("octet-import-pi", self.manifest["name"])
        self.assertEqual("0.3", self.manifest["api_version"])
        self.assertEqual("=0.8.1-rc.1", self.manifest["requires_octet"])
        self.assertFalse(self.manifest["capabilities"]["process"])
        self.assertFalse(self.manifest["capabilities"]["network"])
        self.assertEqual([], self.manifest["contributes"]["tools"])
        replies = self.exchange([
            self.initialize(),
            ("migration/detect", {"source_root": str(self.source)}),
            ("migration/import", {"source_root": str(self.source), "config_paths": ["settings.json"]}),
            ("shutdown", {}),
        ])
        response = api.parse_initialize_response(replies[0]["result"])
        contract = api.negotiate(self.offer, response.contract)
        self.assertIn("migration.adapter.v1", contract.capabilities)
        self.assertEqual([], response.tools)
        detection = api.parse_migration_detect_result(replies[1]["result"])
        self.assertTrue(detection.detected)
        self.assertEqual(["settings.json"], detection.config_paths)
        imported = api.parse_migration_import_result(replies[2]["result"])
        self.assertEqual("openai", imported.models[0].provider)
        self.assertEqual("gpt-4o-mini", imported.models[0].model)
        self.assertEqual("Review before applying.\n", imported.skills[0].content)
        self.assertEqual({"path", "name", "command", "args"}, set(imported.mcp_servers[0].to_wire()))
        self.assertTrue(imported.diagnostics)
        self.assertEqual({"terminal": "shutdown"}, replies[3]["result"])

    def test_source_and_path_rejections_leave_both_setups_untouched(self):
        alias = self.root / "linked-source"
        alias.symlink_to(self.source, target_is_directory=True)
        for method, params in [
            ("migration/detect", {"source_root": "relative"}),
            ("migration/detect", {"source_root": str(alias)}),
            ("migration/import", {"source_root": str(self.source), "config_paths": ["../auth.json"]}),
            ("migration/import", {"source_root": str(self.source), "config_paths": ["auth.json"]}),
        ]:
            with self.subTest(method=method, params=params):
                replies = self.exchange([self.initialize(), (method, params), ("shutdown", {})])
                self.assertEqual({"code": -32602, "message": "invalid params"}, replies[1]["error"])

    def test_detect_requires_negotiation(self):
        replies = self.exchange([("migration/detect", {"source_root": str(self.source)}), ("shutdown", {})])
        self.assertIn("error", replies[0])

    def test_no_match_has_no_destination_writes(self):
        empty = self.root / "empty"
        empty.mkdir()
        replies = self.exchange([self.initialize(), ("migration/detect", {"source_root": str(empty)}), ("shutdown", {})])
        self.assertEqual({"detected": False, "config_paths": [], "diagnostics": []}, replies[1]["result"])

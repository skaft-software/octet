"""The /extensions server manager: guarded edits, live reconcile, and menu."""

from __future__ import annotations

from dataclasses import replace
import json
import os
from pathlib import Path
import random
import stat
import sys
import tempfile
import unittest
from unittest import mock

from octet_mcp.config import BridgeConfig, ConfigError
from octet_mcp.editor import ConfigEditor, EditCancelled, ask_stdio, describe, parse_environment
from octet_mcp.manager import BridgeManager
from octet_mcp.menu import MAX_MENU_SERVERS, build_menu
from octet_mcp.runtime import build_runtime

from .helpers import FakeExtension, limits, real_server_config, server_config, wait_for

OWNER = {
    "resource_owner": {
        "session_id": "session-owner",
        "extension_instance_id": "instance-owner",
        "process_generation": 1,
    }
}


def answers(*values):
    """A scripted guided form: each prompt receives the next answer."""

    queue = list(values)
    prompts = []

    def ask(prompt, secret=False):
        prompts.append((prompt, secret))
        return queue.pop(0)

    ask.prompts = prompts
    return ask


def walk(items, depth=1):
    ids = set()
    recommended = 0
    for item in items:
        assert item["id"] not in ids, item
        ids.add(item["id"])
        recommended += bool(item.get("recommended"))
        assert ("command" in item) != ("items" in item), item
        yield depth, item
        if "items" in item:
            yield from walk(item["items"], depth + 1)
    assert recommended <= 1, items


class EditorTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "mcp.json"
        self.editor = ConfigEditor(self.path, workspace=None, experimental_streamable_http_mcp=False)

    def test_edits_round_trip_through_launch_validation_as_a_private_file(self):
        config = self.editor.add(
            "files",
            {"command": sys.executable, "args": ["-m", "server"], "env": {"TOKEN": "s3cret"}},
            set(),
        )
        self.assertEqual([server.id for server in config.servers], ["files"])
        self.assertEqual(stat.S_IMODE(os.stat(self.path).st_mode), 0o600)
        self.assertFalse(self.editor.set_enabled("files", False).servers[0].enabled)
        self.assertTrue(self.editor.set_enabled("files", True).servers[0].enabled)
        self.assertEqual(self.editor.remove("files").servers, ())
        self.assertEqual(json.loads(self.path.read_text())["servers"], {})
        self.assertEqual(list(Path(self.directory.name).glob(".mcp.*")), [])

    def test_an_invalid_edit_leaves_the_file_untouched(self):
        self.editor.add("files", {"command": sys.executable}, set())
        before = self.path.read_bytes()
        with self.assertRaises(ConfigError):
            self.editor.replace("files", {"command": sys.executable, "cwd": "\x01"})
        with self.assertRaises(ConfigError):
            # A remote server stays refused without the process-owner opt-in.
            self.editor.add("remote", {"transport": "streamable-http",
                                       "url": "https://mcp.example.com/mcp"}, set())
        with self.assertRaises(ConfigError):
            self.editor.add("files", {"command": "other"}, set())
        with self.assertRaises(ConfigError):
            self.editor.remove("from-project")
        self.assertEqual(self.path.read_bytes(), before)
        self.assertEqual(list(Path(self.directory.name).glob(".mcp.*")), [])

    def test_the_guided_stdio_form_keeps_blank_answers_and_hides_the_environment(self):
        ask = answers("npx", "-y '@scope/server' --flag", "API_KEY=abc OTHER='x y'")
        descriptor = ask_stdio(ask)
        self.assertEqual(descriptor, {
            "command": "npx",
            "args": ["-y", "@scope/server", "--flag"],
            "env": {"API_KEY": "abc", "OTHER": "x y"},
        })
        self.assertTrue(ask.prompts[2][1], "environment values are asked as a secret")
        kept = ask_stdio(answers("", "", ""), descriptor)
        self.assertEqual(kept, descriptor)
        self.assertNotIn("abc", describe(descriptor))
        self.assertIn("env API_KEY, OTHER", describe(descriptor))
        with self.assertRaises(EditCancelled):
            ask_stdio(answers(None))
        with self.assertRaises(ConfigError) as caught:
            parse_environment("NOT-VALID=secret-value")
        self.assertNotIn("secret-value", str(caught.exception))


class ReconcileTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.extension = FakeExtension(Path(self.directory.name))
        self.limits = limits(backoff_initial_ms=10, backoff_max_ms=20, shutdown_timeout_ms=500)
        self.manager = BridgeManager(
            self.extension,
            BridgeConfig(servers=(real_server_config(),), limits=self.limits),
            scratch_directory=Path(self.directory.name),
            random_source=random.Random(1),
        )
        self.addCleanup(self.manager.shutdown)
        self.manager.start()
        wait_for(lambda: self.state("real-fixture") == "ready", message="fixture ready")

    def state(self, server_id):
        for server in self.manager.domain_snapshot()["servers"]:
            if server["id"] == server_id:
                return server["state"]
        return None

    def test_added_changed_and_removed_servers_reconcile_without_touching_the_rest(self):
        original = real_server_config()
        added = server_config("stable", server_id="second")
        result = self.manager.apply_config(
            BridgeConfig(servers=(original, added), limits=self.limits)
        )
        self.assertEqual(result, {"added": ["second"], "changed": [], "removed": []})
        wait_for(lambda: self.state("second") == "ready", message="added server ready")
        untouched = self.manager._servers["real-fixture"].client

        disabled = replace(added, enabled=False)
        result = self.manager.apply_config(
            BridgeConfig(servers=(original, disabled), limits=self.limits)
        )
        self.assertEqual(result["changed"], ["second"])
        self.assertEqual(self.state("second"), "stopped")
        self.assertIs(self.manager._servers["real-fixture"].client, untouched)

        result = self.manager.apply_config(BridgeConfig(servers=(original,), limits=self.limits))
        self.assertEqual(result["removed"], ["second"])
        self.assertIsNone(self.state("second"))
        self.assertFalse(any(name.startswith("mcp_second") for name in self.extension._tools))
        self.assertEqual(self.state("real-fixture"), "ready")


class RuntimeEditTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "mcp.json"
        self.path.write_text('{"version": 1, "servers": {}}\n', encoding="utf-8")
        self.path.chmod(0o600)
        self.extension, self.manager = build_runtime(config_path=self.path)
        self.addCleanup(self.manager.shutdown)
        self.command = self.extension._commands["mcp"].handler

    def run_command(self, arguments, *, form=(), confirmed=True):
        with mock.patch.object(self.extension, "request_input", side_effect=answers(*form)), \
             mock.patch.object(self.extension, "confirm", return_value=confirmed) as confirm:
            result = self.command(arguments, OWNER)
        return result["text"], confirm

    def test_a_server_is_added_confirmed_listed_disabled_and_removed(self):
        text, confirm = self.run_command(
            ["add", "stdio"],
            form=("files", sys.executable, "-m files_server", "", "Local files"),
        )
        self.assertIn("Added files", text)
        self.assertIn("Add Local files and start it?", confirm.call_args.args[0])
        self.assertIn(sys.executable, confirm.call_args.kwargs["detail"])
        saved = json.loads(self.path.read_text())["servers"]["files"]
        self.assertEqual(saved["label"], "Local files")
        self.assertEqual([server.id for server in self.manager.config.servers], ["files"])

        menu = self.extension._menu_handler({}, OWNER)
        server = next(item for item in menu["items"] if item["id"] == "server:files")
        actions = {item["id"]: item["arguments"] for item in server["items"]}
        self.assertEqual(actions["disable"], ["disable", "files"])
        self.assertEqual(actions["remove"], ["remove", "files"])
        self.assertTrue(next(item for item in server["items"] if item["id"] == "remove")["destructive"])

        self.assertIn("files disabled", self.run_command(["disable", "files"])[0])
        self.assertFalse(json.loads(self.path.read_text())["servers"]["files"]["enabled"])
        self.assertIn("Removed files", self.run_command(["remove", "files"])[0])
        self.assertEqual(self.manager.config.servers, ())

    def test_cancelled_or_declined_forms_change_nothing(self):
        self.assertEqual(self.run_command(["add", "stdio"], form=(None,))[0], "Nothing was changed.")
        declined = self.run_command(
            ["add", "stdio"], form=("files", sys.executable, "", "", ""), confirmed=False
        )[0]
        self.assertEqual(declined, "Nothing was added.")
        self.assertEqual(json.loads(self.path.read_text())["servers"], {})
        refused = self.run_command(["add", "stdio"], form=("Bad Name",))[0]
        self.assertIn("MCP configuration unchanged", refused)
        self.assertIn("--experimental-streamable-http-mcp", self.run_command(["add", "http"])[0])


class MenuTests(unittest.TestCase):
    def snapshot(self, servers):
        return {"summary": {"degraded": False, "refreshing": False}, "servers": servers}

    def server(self, server_id, state="ready", **values):
        record = {
            "id": server_id, "label": server_id, "state": state, "connected": state == "ready",
            "toolCount": 2, "transport": "stdio", "scope": "user",
            "catalogRevision": 1, "hostCatalogRevision": 1, "restart": {},
            "actions": [
                {"id": "refresh", "enabled": state == "ready"},
                {"id": "restart", "enabled": state in {"ready", "stopped"}},
                {"id": "stop", "enabled": state == "ready"},
            ],
            "tools": [],
        }
        record.update(values)
        return record

    def test_an_empty_bridge_recommends_adding_a_server(self):
        menu = build_menu(self.snapshot([]), user_servers={}, experimental_streamable_http_mcp=False,
                          config_path=Path("/home/me/.octet/mcp.json"))
        self.assertEqual(menu["status"], {"state": "empty", "label": "No servers yet"})
        self.assertEqual(menu["items"][0]["arguments"], ["add", "stdio"])
        self.assertTrue(menu["items"][0]["recommended"])
        self.assertIn("--experimental-streamable-http-mcp", menu["detail"])

    def test_servers_offer_lifecycle_and_only_user_servers_offer_edits(self):
        menu = build_menu(
            self.snapshot([self.server("files"), self.server("shared", "stopped", scope="project")]),
            user_servers={"files": {"command": "x"}},
            experimental_streamable_http_mcp=True,
            config_path=Path("/home/me/.octet/mcp.json"),
        )
        self.assertEqual(menu["status"]["label"], "1/2 connected · 4 tools")
        add = menu["items"][0]
        self.assertEqual([item["arguments"] for item in add["items"]], [["add", "stdio"], ["add", "http"]])
        files = next(item for item in menu["items"] if item["id"] == "server:files")
        self.assertEqual([item["id"] for item in files["items"]],
                         ["show", "refresh", "restart", "stop", "disable", "edit", "remove"])
        shared = next(item for item in menu["items"] if item["id"] == "server:shared")
        self.assertEqual([item["id"] for item in shared["items"]], ["show", "restart"])
        self.assertEqual(shared["items"][1]["label"], "Start")
        self.assertIn("trusted project file", shared["description"])
        for _depth, item in walk(menu["items"]):
            if "command" in item:
                self.assertEqual(item["command"], "mcp")

    def test_a_full_bridge_stays_within_the_hosts_menu_bounds(self):
        servers = [self.server(f"server-{index:02d}") for index in range(32)]
        menu = build_menu(
            self.snapshot(servers),
            user_servers={server["id"]: {} for server in servers},
            experimental_streamable_http_mcp=True,
            config_path=Path("/home/me/.octet/mcp.json"),
        )
        entries = list(walk(menu["items"]))
        self.assertLessEqual(len(entries), 256)
        self.assertLessEqual(max(depth for depth, _ in entries), 4)
        self.assertIn(f"{32 - MAX_MENU_SERVERS} more", menu["detail"])

    def test_a_configuration_that_did_not_load_offers_only_status(self):
        menu = build_menu(
            self.snapshot([]), user_servers={}, experimental_streamable_http_mcp=False,
            config_path=Path("/home/me/.octet/mcp.json"),
            config_error={"code": "invalid_config", "summary": "MCP configuration failed a check"},
        )
        self.assertEqual(menu["status"]["state"], "degraded")
        self.assertEqual([item["arguments"] for item in menu["items"]], [["status"]])


if __name__ == "__main__":
    unittest.main()

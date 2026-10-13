"""The /extensions options menu and the followed browser setup."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from typing import Any, Dict, List, Optional

from octet_browse.controller import BrowseController
from octet_browse.paths import BrowsePaths, PLAYWRIGHT_VERSION
from octet_browse.presentation import BrowsePresentation
from octet_browse.setup import SetupManager, SetupStatus

try:  # the bundle suite discovers tests as a package
    from .helpers import OWNER_CONTEXT
except ImportError:  # pragma: no cover - flat discovery
    from helpers import OWNER_CONTEXT


class ScriptedSetup:
    """Replays setup states, phases, and download progress, one per poll."""

    def __init__(self, script: List[Dict[str, Optional[str]]]) -> None:
        self.script = list(script)
        self.index = 0

    def _step(self) -> Dict[str, Optional[str]]:
        return self.script[min(self.index, len(self.script) - 1)]

    def status(self) -> SetupStatus:
        step = self._step()
        self.index += 1
        return SetupStatus(step["state"] or "", step.get("detail") or "", "~/.octet/browse/install.log")

    def start(self) -> SetupStatus:
        return SetupStatus("installing", "started", "~/.octet/browse/install.log")

    def phase(self) -> Optional[str]:
        return self.script[min(max(self.index - 1, 0), len(self.script) - 1)].get("phase")

    def download_progress(self) -> Optional[str]:
        return self.script[min(max(self.index - 1, 0), len(self.script) - 1)].get("download")

    def shutdown(self, timeout: float = 1.0) -> None:
        pass


class ImmediateCancellation:
    def __init__(self) -> None:
        self.cancelled = False

    def wait(self, _timeout: float) -> bool:
        return self.cancelled

    def raise_if_cancelled(self) -> None:
        if self.cancelled:
            raise RuntimeError("cancelled")


def controller_with(setup: Any, home: str) -> BrowseController:
    presentation = BrowsePresentation(lambda *_: None)
    controller = BrowseController(presentation, paths=BrowsePaths.for_home(Path(home)), setup=setup)
    return controller


def actions(menu: Dict[str, Any]) -> Dict[str, Dict[str, Any]]:
    found = {}
    recommended = 0
    for item in menu["items"]:
        assert item["command"] == "browse", item
        recommended += bool(item.get("recommended"))
        found[item["id"]] = item
    assert recommended <= 1, menu
    return found


class MenuTests(unittest.TestCase):
    def test_a_fresh_install_recommends_following_setup_and_guards_the_reset(self) -> None:
        with tempfile.TemporaryDirectory() as home:
            controller = controller_with(
                SetupManager(BrowsePaths.for_home(Path(home)), installer_hook=lambda *_: None),
                home,
            )
            try:
                menu = controller.menu(OWNER_CONTEXT)
            finally:
                controller.shutdown()
        self.assertEqual(menu["status"], {"state": "empty", "label": "Not set up"})
        items = actions(menu)
        self.assertEqual(items["setup"]["arguments"], ["setup", "--wait"])
        self.assertTrue(items["setup"]["recommended"])
        self.assertIn(PLAYWRIGHT_VERSION, items["setup"]["description"])
        self.assertTrue(items["reset"]["destructive"])
        self.assertNotIn("open", items)

    def test_a_ready_browser_offers_open_or_close_for_this_owner_only(self) -> None:
        with tempfile.TemporaryDirectory() as home:
            controller = controller_with(ScriptedSetup([{"state": "ready", "detail": "ready"}]), home)
            try:
                closed = actions(controller.menu(OWNER_CONTEXT))
                controller.presentation.update_browser(
                    {"open": True, "isolated_open": True,
                     "tabs": [{"tab_id": "a"}, {"tab_id": "b"}]},
                    resource_owner=OWNER_CONTEXT["resource_owner"],
                )
                open_menu = controller.menu(OWNER_CONTEXT)
                other = dict(OWNER_CONTEXT, resource_owner=dict(
                    OWNER_CONTEXT["resource_owner"], session_id="another-session"))
                elsewhere = actions(controller.menu(other))
            finally:
                controller.shutdown()
        self.assertTrue(closed["open"]["recommended"])
        self.assertEqual(open_menu["status"], {"state": "active", "label": "Open · 2 tabs"})
        self.assertIn("close", actions(open_menu))
        self.assertIn("open", elsewhere)

    def test_following_setup_reports_each_step_until_it_is_ready(self) -> None:
        with tempfile.TemporaryDirectory() as home:
            setup = ScriptedSetup([
                {"state": "installing", "phase": "Installing Playwright " + PLAYWRIGHT_VERSION},
                {"state": "installing", "phase": "Downloading Chromium", "download": "40% of 172.1 MiB"},
                {"state": "installing", "phase": "Downloading Chromium", "download": "40% of 172.1 MiB"},
                {"state": "ready", "detail": "ready"},
            ])
            controller = controller_with(setup, home)
            steps: List[str] = []
            try:
                text = controller.command(
                    ["setup", "--wait"], OWNER_CONTEXT, lambda *_: True,
                    cancellation=ImmediateCancellation(), progress=steps.append,
                )
            finally:
                controller.shutdown()
        self.assertEqual(steps, [
            "Installing Playwright " + PLAYWRIGHT_VERSION,
            "Downloading Chromium · 40% of 172.1 MiB",
        ])
        self.assertIn("are ready", text)

    def test_a_failed_setup_names_the_log_and_cancelling_stops_only_the_report(self) -> None:
        with tempfile.TemporaryDirectory() as home:
            failed = controller_with(ScriptedSetup([
                {"state": "installing", "phase": "Downloading Chromium"},
                {"state": "degraded", "detail": "Pinned browser setup failed."},
            ]), home)
            cancelled = controller_with(ScriptedSetup([{"state": "installing", "phase": "Preparing"}]), home)
            token = ImmediateCancellation()
            token.cancelled = True
            try:
                text = failed.command(["setup", "--wait"], OWNER_CONTEXT, lambda *_: True,
                                      cancellation=ImmediateCancellation())
                with self.assertRaises(RuntimeError):
                    cancelled.command(["setup", "--wait"], OWNER_CONTEXT, lambda *_: True,
                                      cancellation=token)
            finally:
                failed.shutdown()
                cancelled.shutdown()
        self.assertIn("Pinned browser setup failed.", text)
        self.assertIn("install.log", text)

    def test_other_argument_shapes_are_still_refused(self) -> None:
        from octet_browse.safety import BrowseError

        with tempfile.TemporaryDirectory() as home:
            controller = controller_with(ScriptedSetup([{"state": "ready"}]), home)
            try:
                for arguments in (["open", "--wait"], ["setup", "--now"], ["a", "b", "c"]):
                    with self.assertRaises(BrowseError):
                        controller.command(arguments, OWNER_CONTEXT, lambda *_: True)
            finally:
                controller.shutdown()


class DownloadProgressTests(unittest.TestCase):
    def test_only_the_percentage_and_size_are_read_back_while_chromium_downloads(self) -> None:
        with tempfile.TemporaryDirectory() as home:
            paths = BrowsePaths.for_home(Path(home))
            paths.ensure_root()
            paths.install_log.write_text(
                "Downloading Chromium from https://example.invalid/secret?token=x\n"
                "|■■■■■■■■                                  |  20% of 172.1 MiB\n"
                "|■■■■■■■■■■■■■■■■                          |  40% of 172.1 MiB\n",
                encoding="utf-8",
            )
            manager = SetupManager(paths, installer_hook=lambda *_: None)
            self.assertIsNone(manager.download_progress())
            manager._set_phase("Downloading Chromium")
            self.assertEqual(manager.download_progress(), "40% of 172.1 MiB")
            manager._set_phase(None)
            self.assertIsNone(manager.phase())


class InstallPhaseTests(unittest.TestCase):
    def test_a_real_install_walks_through_named_phases(self) -> None:
        seen: List[Optional[str]] = []

        with tempfile.TemporaryDirectory() as home:
            paths = BrowsePaths.for_home(Path(home))
            manager = SetupManager(paths)
            commands = []

            def fake_run(arguments, log, *, env):
                commands.append(arguments)
                seen.append(manager.phase())

            manager._run_command = fake_run  # type: ignore[method-assign]
            with open(Path(home) / "log", "w", encoding="utf-8") as log:
                manager._install_pinned_runtime(Path(home) / "runtime", log)
        self.assertEqual(seen, [
            "Creating the browser environment",
            f"Installing Playwright {PLAYWRIGHT_VERSION}",
            "Downloading Chromium",
        ])
        self.assertEqual(len(commands), 3)


if __name__ == "__main__":
    unittest.main()

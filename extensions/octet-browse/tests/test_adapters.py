"""Explicit target/native fail-closed regression tests; no real browsers."""

from __future__ import annotations

import tempfile
import time
import unittest
from dataclasses import replace
from pathlib import Path
from unittest.mock import Mock

from octet_browse.adapters import (
    AdapterRegistry,
    NativeFirefoxConnector,
    NativeSafariConnector,
    PlaywrightConnector,
    TargetSelection,
)
from octet_browse.paths import BrowsePaths
from octet_browse.safety import BrowseError, ResourceOwner
from octet_browse.worker import BrowserEngine, OperationContext
from tests.helpers import FakeElement, FakePage


class ScriptedAction(FakeElement):
    """An otherwise innocuous button whose handler navigates or opens a popup."""

    def __init__(self, page, effect, guard):
        super().__init__("Inspect", attrs={"aria-label": "Inspect"})
        self.page = page
        self.effect = effect
        self.guard = guard
        self.attempts = 0
        self.effects = 0

    def click(self, timeout=None):
        self.attempts += 1
        # A real bridge must block at the request/target/download boundary,
        # before the script can navigate or create a new target.
        if self.guard():
            raise BrowseError("navigation_blocked", "The preventive boundary blocked the scripted action.")
        self.effects += 1
        if self.effect == "redirect":
            self.page.url = "file:///sensitive"
        else:
            self.page.popup_created = True



class ExplicitConnectorTests(unittest.TestCase):
    def setUp(self):
        self.owner = ResourceOwner("session", "instance", 1)
        self.selection = TargetSelection("bridge", "browser-1", "session-1", "window-1", "tab-1")

    def test_native_descriptors_never_invoke_selector_or_substitute_chromium(self):
        for constructor in (NativeFirefoxConnector, NativeSafariConnector):
            for browser_id in ("browser-1", "firefox", "safari"):
                with self.subTest(connector=constructor.__name__, browser_id=browser_id):
                    selector = Mock(side_effect=AssertionError("native selector must not run"))
                    connector = constructor("bridge", selector=selector, capabilities={"click": True})
                    self.assertEqual(connector.describe()["state"], "unsupported")
                    self.assertTrue(all(not value["supported"] for value in connector.capabilities.values()))
                    with self.assertRaises(BrowseError) as raised:
                        connector.select(replace(self.selection, browser_id=browser_id), self.owner)
                    self.assertEqual(raised.exception.code, "unsupported_capability")
                    selector.assert_not_called()

    def test_literal_native_selection_cannot_route_to_chromium(self):
        selector = Mock()
        connector = PlaywrightConnector("bridge", selector=selector)
        for family in ("firefox", "gecko", "safari", "webkit"):
            with self.subTest(family=family), self.assertRaises(BrowseError) as raised:
                connector.select(replace(self.selection, browser_id=family), self.owner)
            self.assertEqual(raised.exception.code, "unsupported_capability")
        selector.assert_not_called()

    def test_missing_identity_never_defaults_to_active_tab(self):
        for field in ("connector_id", "browser_id", "session_id", "window_id", "tab_id"):
            values = self.selection.as_dict()
            values.pop(field)
            with self.subTest(field=field), self.assertRaises(BrowseError) as raised:
                TargetSelection.from_mapping(values)
            self.assertEqual(raised.exception.code, "invalid_backend_selection")

    def test_changed_identity_family_and_revision_are_rejected(self):
        page = FakePage()
        selected = replace(self.selection, target_revision="revision-1")
        for changes, code in [
            ({"selection": replace(selected, tab_id="other").as_dict()}, "backend_invalid_target"),
            ({"browser_family": "firefox"}, "backend_invalid_target"),
            ({"selection": self.selection.as_dict(), "target_revision": "revision-2"}, "stale_target"),
        ]:
            value = {"selection": selected.as_dict(), "page": page, **changes}
            connector = PlaywrightConnector("bridge", selector=lambda *_: value)
            with self.subTest(changes=changes), self.assertRaises(BrowseError) as raised:
                connector.select(selected, self.owner)
            self.assertEqual(raised.exception.code, code)

    def test_unqualified_external_actions_are_off_even_when_declared(self):
        page = FakePage()
        action = ScriptedAction(page, "redirect", lambda: False)
        page.role_elements["button"] = [action]
        connector = PlaywrightConnector(
            "bridge",
            selector=lambda *_: {"selection": self.selection.as_dict(), "page": page},
            capabilities={name: True for name in (
                "click", "type", "press", "scroll", "wait", "navigation",
                "tab_close", "new_tab", "popup",
            )},
        )
        self.assertTrue(connector.capabilities["snapshot"]["supported"])
        self.assertTrue(connector.capabilities["screenshot"]["supported"])
        for name in ("click", "type", "press", "scroll", "wait", "navigation", "tab_close", "new_tab", "popup"):
            self.assertFalse(connector.capabilities[name]["supported"], name)
        with tempfile.TemporaryDirectory() as home:
            engine = BrowserEngine(BrowsePaths.for_home(Path(home)), None, None, adapters=AdapterRegistry([connector]))
            operation = lambda: OperationContext(time.monotonic() + 2)
            try:
                tab_id = engine.backend_select(operation(), self.owner, self.selection)["affected_tab_id"]
                engine.snapshot(operation(), self.owner, tab_id)
                with self.assertRaises(BrowseError) as raised:
                    engine.click(operation(), self.owner, tab_id, 'role=button[name="Inspect"]', None, lambda *_: True)
                self.assertEqual(raised.exception.code, "unsupported_capability")
                self.assertEqual(action.attempts, 0)
                self.assertEqual(page.url, "https://example.test/")
            finally:
                engine.shutdown()

    def test_guarded_connector_blocks_scripted_redirect_and_popup_before_effect(self):
        for effect in ("redirect", "popup"):
            with self.subTest(effect=effect), tempfile.TemporaryDirectory() as home:
                page = FakePage()
                active = {"guard": False}
                action = ScriptedAction(page, effect, lambda: active["guard"])
                page.role_elements["button"] = [action]
                checks = []

                def enforce(target, owner):
                    self.assertIs(target.page, page)
                    self.assertEqual(owner, self.owner)
                    checks.append(True)
                    active["guard"] = True
                    return True

                connector = PlaywrightConnector(
                    "bridge",
                    selector=lambda *_: {"selection": self.selection.as_dict(), "page": page},
                    enforce_boundary=enforce,
                    capabilities={"click": True, "popup": True},
                )
                engine = BrowserEngine(BrowsePaths.for_home(Path(home)), None, None, adapters=AdapterRegistry([connector]))
                operation = lambda: OperationContext(time.monotonic() + 2)
                try:
                    tab_id = engine.backend_select(operation(), self.owner, self.selection)["affected_tab_id"]
                    # No href/form metadata advertises this scripted behavior.
                    with self.assertRaises(BrowseError) as raised:
                        engine.click(operation(), self.owner, tab_id, 'role=button[name="Inspect"]', None, lambda *_: True)
                    self.assertEqual(raised.exception.code, "navigation_blocked")
                    self.assertEqual(action.attempts, 1)
                    self.assertEqual(action.effects, 0)
                    self.assertEqual(page.url, "https://example.test/")
                    self.assertFalse(getattr(page, "popup_created", False))
                    self.assertGreaterEqual(len(checks), 2)  # selection and action-time guard
                finally:
                    engine.shutdown()

    def test_boundary_loss_fails_before_external_click(self):
        page = FakePage()
        action = ScriptedAction(page, "popup", lambda: False)
        page.role_elements["button"] = [action]
        active = {"guard": True}
        connector = PlaywrightConnector(
            "bridge",
            selector=lambda *_: {"selection": self.selection.as_dict(), "page": page},
            enforce_boundary=lambda *_: active["guard"],
            capabilities={"click": True},
        )
        with tempfile.TemporaryDirectory() as home:
            engine = BrowserEngine(BrowsePaths.for_home(Path(home)), None, None, adapters=AdapterRegistry([connector]))
            operation = lambda: OperationContext(time.monotonic() + 2)
            try:
                tab_id = engine.backend_select(operation(), self.owner, self.selection)["affected_tab_id"]
                active["guard"] = False
                with self.assertRaises(BrowseError) as raised:
                    engine.click(operation(), self.owner, tab_id, 'role=button[name="Inspect"]', None, lambda *_: True)
                self.assertEqual(raised.exception.code, "backend_boundary_failed")
                self.assertEqual(action.attempts, 0)
            finally:
                engine.shutdown()

    def test_confirmation_rechecks_external_boundary_before_click_or_press(self):
        for action in ("click", "press"):
            with self.subTest(action=action), tempfile.TemporaryDirectory() as home:
                form = FakeElement("Publish post", attrs={"method": "post", "action": "/publish"})
                button = FakeElement("Publish", attrs={"type": "submit", "aria-label": "Publish"}, form=form)
                page = FakePage()
                page.role_elements["button"] = [button]
                active = {"guard": True}
                connector = PlaywrightConnector(
                    "bridge",
                    selector=lambda *_: {"selection": self.selection.as_dict(), "page": page},
                    enforce_boundary=lambda *_: active["guard"],
                    capabilities={"click": True, "press": True},
                )
                engine = BrowserEngine(BrowsePaths.for_home(Path(home)), None, None, adapters=AdapterRegistry([connector]))
                operation = lambda: OperationContext(time.monotonic() + 2)

                def confirm(*_):
                    active["guard"] = False
                    return True

                try:
                    tab_id = engine.backend_select(operation(), self.owner, self.selection)["affected_tab_id"]
                    args = (operation(), self.owner, tab_id, 'role=button[name="Publish"]', None)
                    with self.assertRaises(BrowseError) as raised:
                        if action == "click":
                            engine.click(*args, confirm)
                        else:
                            engine.press(*args, "Enter", confirm)
                    self.assertEqual(raised.exception.code, "backend_boundary_failed")
                    self.assertEqual(button.clicked, 0)
                    self.assertEqual(button.pressed, [])
                finally:
                    engine.shutdown()

    def test_boundary_failure_releases_selected_target(self):
        page = FakePage()
        release = Mock()
        connector = PlaywrightConnector(
            "bridge",
            selector=lambda *_: {"selection": self.selection.as_dict(), "page": page},
            enforce_boundary=lambda *_: False,
            release=release,
            capabilities={"click": True},
        )
        with tempfile.TemporaryDirectory() as home:
            registry = AdapterRegistry([connector])
            engine = BrowserEngine(BrowsePaths.for_home(Path(home)), None, None, adapters=registry)
            try:
                with self.assertRaises(BrowseError) as raised:
                    engine.backend_select(OperationContext(time.monotonic() + 2), self.owner, self.selection)
                self.assertEqual(raised.exception.code, "backend_boundary_failed")
                release.assert_called_once()
                self.assertIsNone(engine._attached)
                other = ResourceOwner("other", "instance", 1)
                registry.claim(self.selection, other)
                registry.release(self.selection, other)
            finally:
                engine.shutdown()

    def test_other_owner_cannot_query_or_operate_selected_target(self):
        with tempfile.TemporaryDirectory() as home:
            page = FakePage()
            verifier = Mock(return_value=True)
            selector = Mock(return_value={"selection": self.selection.as_dict(), "page": page})
            connector = PlaywrightConnector("bridge", selector=selector, verify_target=verifier)
            registry = AdapterRegistry([connector])
            engine = BrowserEngine(BrowsePaths.for_home(Path(home)), None, None, adapters=registry)
            operation = lambda: OperationContext(time.monotonic() + 2)
            try:
                selected = engine.backend_select(operation(), self.owner, self.selection)
                verifier.reset_mock()
                other = ResourceOwner("other", "instance", 1)
                status = engine.status(operation(), other)
                self.assertEqual(status["tabs"], [])
                self.assertIsNone(status["selected_backend"])
                with self.assertRaises(BrowseError) as raised:
                    engine.snapshot(operation(), other, selected["affected_tab_id"])
                self.assertEqual(raised.exception.code, "owner_mismatch")
                verifier.assert_not_called()
                self.assertEqual(selector.call_count, 1)
                with self.assertRaises(BrowseError) as claim:
                    registry.claim(self.selection, other)
                self.assertEqual(claim.exception.code, "backend_in_use")
            finally:
                engine.shutdown()


if __name__ == "__main__":
    unittest.main()

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
from tests.helpers import FakePage


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

"""Non-activating target creation fixtures; not physical desktop qualification."""

from __future__ import annotations

import tempfile
import unittest

from octet_browse.safety import BrowseError, ResourceOwner
from octet_browse.worker import MAX_TABS
from tests.helpers import FakePage
from tests import test_worker as fixtures


class NavigablePage(FakePage):
    def __init__(self, target_id: str):
        super().__init__(url="about:blank")
        self.target_id = target_id
        self.navigations = []

    def goto(self, url: str, **_arguments: object) -> None:
        self.url = url
        self.navigations.append(url)


class PageSession:
    def __init__(self, context, page):
        self.context = context
        self.page = page
        self.detached = False

    def send(self, method):
        assert method == "Target.getTargetInfo"
        if self.context.arrival_during_probe:
            self.context.arrival_during_probe = False
            self.context.emit(self.context.created)
        return {"targetInfo": {"targetId": self.page.target_id}}

    def detach(self):
        self.detached = True


class TargetSession:
    def __init__(self, context):
        self.context = context
        self.detached = False

    def send(self, method, arguments):
        context = self.context
        context.commands.append((method, arguments))
        if method == "Target.createTarget":
            if context.fail_create:
                raise RuntimeError("private transport detail")
            assert arguments == {"url": "about:blank", "background": True}
            context.created = NavigablePage("created")
            if context.interloper:
                context.emit(context.interloper)
            if not context.delayed and not context.arrival_during_probe:
                context.emit(context.created)
            if context.cancel_on_create:
                context.cancel_on_create.abandoned.set()
            return {"targetId": "created"}
        if method == "Target.closeTarget":
            assert arguments == {"targetId": "created"}
            context.created.close()
            return {"success": True}
        raise AssertionError(method)

    def detach(self):
        self.detached = True


class BackgroundContext(fixtures.LifecycleContext):
    def __init__(self, pages):
        super().__init__(pages)
        self.browser = self
        self.commands = []
        self.sessions = []
        self.created = None
        self.interloper = None
        self.delayed = False
        self.arrival_during_probe = False
        self.fail_create = False
        self.cancel_on_create = None
        self.waits = []

    def new_page(self):
        raise AssertionError("activating fallback must never run")

    def new_browser_cdp_session(self):
        session = TargetSession(self)
        self.sessions.append(session)
        return session

    def new_cdp_session(self, page):
        session = PageSession(self, page)
        self.sessions.append(session)
        return session

    def emit(self, page):
        self.pages.append(page)
        for handler in self.handlers.get("page", []):
            handler(page)

    def wait_for_event(self, event, *, timeout):
        assert event == "page"
        assert 0 < timeout <= 2000
        self.waits.append(timeout)
        if not self.delayed:
            raise AssertionError("missed page already delivered during a probe")
        self.delayed = False
        self.emit(self.created)
        return self.created


class BackgroundTabTests(unittest.TestCase):
    operation = staticmethod(fixtures.BrowserLifecycleTests._operation)

    def setUp(self):
        home = tempfile.TemporaryDirectory()
        self.addCleanup(home.cleanup)
        self.initial = NavigablePage("initial")
        self.context = BackgroundContext([self.initial])
        self.engine, self.playwright, self.owner, self.executable = fixtures.BrowserLifecycleTests._engine(
            home.name, [self.context]
        )
        self.addCleanup(self.engine.shutdown)
        self.launch = self.engine.launch(self.operation(), self.owner)

    def test_launch_keeps_visible_isolated_profile_and_default_safeguards(self):
        arguments = self.playwright.chromium.launch_arguments
        self.assertIs(arguments["headless"], False)
        self.assertIs(arguments["accept_downloads"], False)
        self.assertEqual(arguments["executable_path"], str(self.executable))
        self.assertEqual(arguments["user_data_dir"], str(self.engine.profiles.paths.profile))
        self.assertNotIn("ignore_default_args", arguments)
        self.assertNotIn("args", arguments)

    def test_new_url_requests_background_target_and_keeps_existing_page(self):
        result = self.engine.open_url(self.operation(), self.owner, "https://example.test/", None)
        self.assertEqual(self.context.commands, [
            ("Target.createTarget", {"url": "about:blank", "background": True})
        ])
        self.assertIs(self.engine._tabs[result["affected_tab_id"]].page, self.context.created)
        self.assertEqual(self.initial.navigations, [])
        self.assertEqual(self.context.created.navigations, ["https://example.test/"])
        self.assertEqual(self.playwright.chromium.launch_calls, 1)
        self.assertTrue(all(session.detached for session in self.context.sessions))

    def test_exact_target_wins_over_unrelated_page_event(self):
        self.context.interloper = NavigablePage("unrelated")
        result = self.engine.open_url(self.operation(), self.owner, "https://example.test/", None)
        self.assertIs(self.engine._tabs[result["affected_tab_id"]].page, self.context.created)
        self.assertEqual(self.context.interloper.navigations, [])
        self.assertFalse(self.context.interloper.is_closed())

    def test_delayed_page_is_waited_for_with_operation_budget(self):
        self.context.delayed = True
        self.engine.open_url(self.operation(), self.owner, "https://example.test/", None)
        self.assertEqual(len(self.context.waits), 1)
        self.assertEqual(self.context.created.navigations, ["https://example.test/"])

    def test_unmatched_popup_then_delayed_target_pumps_events_once(self):
        self.context.interloper = NavigablePage("unrelated")
        self.context.delayed = True
        result = self.engine.open_url(self.operation(), self.owner, "https://example.test/", None)
        self.assertEqual(len(self.context.waits), 1)
        self.assertIs(self.engine._tabs[result["affected_tab_id"]].page, self.context.created)
        self.assertEqual(self.context.interloper.navigations, [])

    def test_missing_browser_handle_fails_closed_without_foreground_fallback(self):
        self.context.browser = None
        with self.assertRaises(BrowseError) as raised:
            self.engine.open_url(self.operation(), self.owner, "https://example.test/", None)
        self.assertEqual(raised.exception.code, "tab_create_failed")
        self.assertEqual(self.context.commands, [])
        self.assertTrue(self.context.closed)
        self.assertIsNone(self.engine._context)
        self.assertTrue(self.engine.status(self.operation(), self.owner)["degraded"])
        self.assertEqual(self.playwright.stop_calls, 1)

    def test_initial_launch_with_no_page_uses_nonactivating_creation(self):
        self.engine.close(self.operation(), self.owner)
        empty = BackgroundContext([])
        self.playwright.chromium.contexts.append(empty)
        result = self.engine.launch(self.operation(), self.owner)
        self.assertTrue(result["open"])
        self.assertEqual(result["tab_count"], 1)
        self.assertEqual(empty.commands, [
            ("Target.createTarget", {"url": "about:blank", "background": True})
        ])
        self.assertTrue(all(session.detached for session in empty.sessions))

    def test_page_arriving_during_probe_is_not_missed(self):
        self.context.interloper = NavigablePage("unrelated")
        self.context.arrival_during_probe = True
        self.engine.open_url(self.operation(), self.owner, "https://example.test/", None)
        self.assertEqual(self.context.waits, [])
        self.assertEqual(self.context.created.navigations, ["https://example.test/"])

    def test_cancellation_closes_only_exact_created_target_and_detaches_sessions(self):
        operation = self.operation()
        self.context.interloper = NavigablePage("unrelated")
        self.context.cancel_on_create = operation
        with self.assertRaises(BrowseError) as raised:
            self.engine.open_url(operation, self.owner, "https://example.test/", None)
        self.assertEqual(raised.exception.code, "operation_timeout")
        self.assertTrue(self.context.created.is_closed())
        self.assertEqual(self.context.created.navigations, [])
        self.assertFalse(self.initial.is_closed())
        self.assertFalse(self.context.interloper.is_closed())
        self.assertFalse(self.context.closed)
        self.assertTrue(all(session.detached for session in self.context.sessions))
        self.assertNotIn(self.context.created, [tab.page for tab in self.engine._tabs.values()])

    def test_unknown_creation_outcome_closes_isolated_context_without_fallback(self):
        self.context.fail_create = True
        with self.assertRaises(BrowseError) as raised:
            self.engine.open_url(self.operation(), self.owner, "https://example.test/", None)
        self.assertEqual(raised.exception.code, "tab_create_failed")
        self.assertNotIn("private transport detail", raised.exception.message)
        self.assertTrue(self.context.closed)
        self.assertIsNone(self.engine._context)
        self.assertEqual(self.playwright.stop_calls, 1)
        self.assertTrue(all(session.detached for session in self.context.sessions))

    def test_existing_tab_navigation_and_repeated_launch_do_not_create_or_activate(self):
        tab_id = self.launch["selected_tab_id"]
        self.engine.open_url(self.operation(), self.owner, "https://example.test/", tab_id)
        self.engine.launch(self.operation(), self.owner)
        self.assertEqual(self.context.commands, [])
        self.assertEqual(self.playwright.chromium.launch_calls, 1)

    def test_owner_and_url_validation_precede_target_creation(self):
        for owner, url, code in [
            (ResourceOwner("other", "instance", 1), "https://example.test/", "owner_mismatch"),
            (self.owner, "file:///tmp/private", "invalid_url"),
        ]:
            with self.subTest(code=code), self.assertRaises(BrowseError) as raised:
                self.engine.open_url(self.operation(), owner, url, None)
            self.assertEqual(raised.exception.code, code)
        self.assertEqual(self.context.commands, [])

    def test_tab_limit_refuses_before_creating_a_visible_tab(self):
        for index in range(MAX_TABS - 1):
            self.context.emit(NavigablePage(f"existing-{index}"))
        with self.assertRaises(BrowseError) as raised:
            self.engine.open_url(self.operation(), self.owner, "https://example.test/", None)
        self.assertEqual(raised.exception.code, "tab_limit")
        self.assertEqual(self.context.commands, [])

    def test_empty_context_repeated_open_uses_nonactivating_creation(self):
        self.initial.close()
        self.context.pages.clear()
        result = self.engine.launch(self.operation(), self.owner)
        self.assertEqual(result["tab_count"], 1)
        self.assertEqual(self.context.commands[0], (
            "Target.createTarget", {"url": "about:blank", "background": True}
        ))
        self.assertEqual(self.playwright.chromium.launch_calls, 1)


if __name__ == "__main__":
    unittest.main()

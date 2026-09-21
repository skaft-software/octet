"""Opt-in real Playwright tests against a loopback-only HTTP fixture server."""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from octet_browse.artifacts import PNG_SIGNATURE, READ_IMAGE_LIMIT
from octet_browse.paths import BrowsePaths
from octet_browse.profile import ProfileManager
from octet_browse.safety import BrowseError, ResourceOwner
from octet_browse.setup import SetupManager
from octet_browse.snapshot import MAX_BODY_SOURCE_CHARS, MAX_BODY_TRAVERSAL_NODES, snapshot_page
from octet_browse.worker import BrowserEngine, OperationContext, PlaywrightWorker


HTML = b"""<!doctype html><html><head><title>Local fixture</title></head><body>
<h1>Local fixture page</h1>
<a id="popup" href="/popup" target="_blank">Open popup</a>
<a id="download" href="/download" download>Download fixture</a>
<input aria-label="Search" type="text">
<input aria-label="Password" type="password" autocomplete="current-password">
<form method="post" action="/publish"><button aria-label="Publish" type="submit">Publish</button></form>
</body></html>"""


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        if self.path == "/redirect":
            self.send_response(302)
            self.send_header("Location", "/final")
            self.end_headers()
            return
        if self.path == "/bad-redirect":
            self.send_response(302)
            self.send_header("Location", "file:///etc/passwd")
            self.end_headers()
            return
        if self.path == "/download":
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Disposition", "attachment; filename=fixture.bin")
            self.end_headers()
            self.wfile.write(b"blocked download")
            return
        body = HTML if self.path == "/" else b"<html><head><title>Second</title></head><body>Second page</body></html>"
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:
        body = b"<html><body>Published</body></html>"
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format: str, *_arguments: object) -> None:
        pass


@unittest.skipUnless(
    os.environ.get("OCTET_BROWSE_PLAYWRIGHT_TESTS") == "1",
    "set OCTET_BROWSE_PLAYWRIGHT_TESTS=1 after /browse setup for real headful integration",
)
class PlaywrightIntegrationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.runtime_paths = BrowsePaths.for_home()
        cls.setup = SetupManager(cls.runtime_paths)
        try:
            cls.setup.validate_runtime()
        except BrowseError as error:
            raise unittest.SkipTest(
                f"pinned runtime unavailable ({error.code}); run confirmed /browse setup first"
            )
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.server_thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.server_thread.start()
        cls.origin = f"http://127.0.0.1:{cls.server.server_port}"

    @classmethod
    def tearDownClass(cls) -> None:
        if hasattr(cls, "server"):
            cls.server.shutdown()
            cls.server.server_close()
            cls.server_thread.join(timeout=2)

    def test_local_navigation_tabs_refs_auth_download_screenshot_and_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as home:
            profile_paths = BrowsePaths.for_home(Path(home))
            profiles = ProfileManager(profile_paths)
            worker = PlaywrightWorker(
                lambda: BrowserEngine(self.runtime_paths, self.setup, profiles)
            )
            owner = ResourceOwner("integration-session", "integration-instance", 1)
            try:
                launch = worker.call("launch", owner, timeout=25)
                self.assertTrue(launch["open"])
                tab_id = launch["selected_tab_id"]
                self.assertIsInstance(tab_id, str)
                repeated_launch = worker.call("launch", owner, timeout=25)
                self.assertEqual(repeated_launch["selected_tab_id"], tab_id)
                self.assertEqual(repeated_launch["tab_count"], 1)
                background = worker.call("open_url", owner, self.origin + "/background", None)
                background_id = background["affected_tab_id"]
                self.assertNotEqual(background_id, tab_id)
                self.assertEqual(background["created_tab_ids"], [background_id])
                observed = worker.call("snapshot", owner, background_id)
                self.assertIn("Second page", observed["text"])
                worker.call("close_tab", owner, background_id)

                opened = worker.call("open_url", owner, self.origin + "/", tab_id, timeout=20)
                self.assertEqual(opened["affected_tab_id"], tab_id)
                snapshot = worker.call("snapshot", owner, tab_id)
                self.assertIn("BEGIN UNTRUSTED BROWSER CONTENT", snapshot["text"])
                self.assertIn("snapshot_generation", snapshot)
                generation = snapshot["snapshot_generation"]
                idle = worker.call("wait", owner, tab_id, 100)
                self.assertEqual(idle["affected_tab_id"], tab_id)
                self.assertTrue(idle["open"])

                typed = worker.call(
                    "type_text",
                    owner,
                    tab_id,
                    'role=textbox[name="Search"]',
                    None,
                    "private integration value",
                )
                self.assertNotIn("private integration value", typed["text"])
                with self.assertRaises(BrowseError) as auth:
                    worker.call(
                        "type_text",
                        owner,
                        tab_id,
                        "css=input[type=password]",
                        None,
                        "never type this",
                    )
                self.assertEqual(auth.exception.code, "manual_auth_required")

                popup = worker.call(
                    "click", owner, tab_id, "css=#popup", None, lambda *_: True, timeout=15
                )
                self.assertEqual(len(popup["created_tab_ids"]), 1)
                popup_id = popup["created_tab_ids"][0]
                self.assertNotEqual(popup_id, tab_id)

                download = worker.call(
                    "click", owner, tab_id, "css=#download", None, lambda *_: True
                )
                self.assertTrue(download["download_blocked"])

                with self.assertRaises(BrowseError) as denied:
                    worker.call(
                        "click",
                        owner,
                        tab_id,
                        'role=button[name="Publish"]',
                        None,
                        lambda *_: False,
                    )
                self.assertEqual(denied.exception.code, "confirmation_denied")

                redirected = worker.call(
                    "open_url", owner, self.origin + "/redirect", tab_id, timeout=20
                )
                self.assertEqual(redirected["affected_tab_id"], tab_id)
                with self.assertRaises(BrowseError) as stale:
                    worker.call("click", owner, tab_id, "ref=e1", generation, lambda *_: True)
                self.assertIn(stale.exception.code, {"stale_snapshot", "stale_reference"})

                with self.assertRaises(BrowseError) as blocked:
                    worker.call(
                        "open_url", owner, self.origin + "/bad-redirect", tab_id, timeout=20
                    )
                self.assertIn(blocked.exception.code, {"navigation_blocked", "navigation_failed"})

                screenshot = worker.call("screenshot", owner, popup_id)
                self.assertTrue(screenshot["data"].startswith(PNG_SIGNATURE))
                self.assertLess(len(screenshot["data"]), READ_IMAGE_LIMIT)

                closed = worker.call("close_tab", owner, popup_id)
                self.assertEqual(closed["closed_tab_ids"], [popup_id])
                worker.call("close", owner)
                self.assertTrue(profiles.reset())
                self.assertFalse(profile_paths.profile.exists())
            finally:
                worker.shutdown(timeout=2)

    @unittest.skipUnless(
        sys.platform == "darwin" and os.environ.get("OCTET_BROWSE_FOCUS_TESTS") == "1",
        "manual opt-in macOS terminal-focus qualification (see QUALIFICATION.md)",
    )
    def test_post_launch_operations_retain_explicit_terminal_focus(self) -> None:
        # This probe reads only the frontmost application PID, never its windows,
        # browser tabs or content. It never activates any application itself.
        self.assertTrue(sys.stdin.isatty(), "focus qualification requires a physical terminal")
        expected_pid = int(os.environ["OCTET_BROWSE_FOCUS_TERMINAL_PID"])
        self.assertGreater(expected_pid, 0)

        def assert_terminal_focus() -> None:
            result = subprocess.run(
                ["/usr/bin/osascript", "-l", "JavaScript", "-e",
                 "ObjC.import('AppKit'); $.NSWorkspace.sharedWorkspace.frontmostApplication.processIdentifier"],
                capture_output=True, text=True, check=True, timeout=3,
            )
            self.assertEqual(int(result.stdout.strip()), expected_pid, "terminal lost foreground focus")

        with tempfile.TemporaryDirectory() as home:
            profiles = ProfileManager(BrowsePaths.for_home(Path(home)))
            worker = PlaywrightWorker(lambda: BrowserEngine(self.runtime_paths, self.setup, profiles))
            owner = ResourceOwner("focus-session", "focus-instance", 1)
            try:
                worker.call("launch", owner, timeout=25)
                input("Leave Chromium visible, manually focus this terminal, then press Enter: ")
                assert_terminal_focus()

                def call(method, *arguments):
                    assert_terminal_focus()
                    result = worker.call(method, owner, *arguments)
                    assert_terminal_focus()
                    return result

                for _ in range(5):
                    call("launch")
                    opened = call("open_url", self.origin + "/background", None)
                    tab_id = opened["affected_tab_id"]
                    call("snapshot", tab_id)
                    call("wait", tab_id, 100)
                    call("screenshot", tab_id)
                    call("open_url", self.origin + "/final", tab_id)
                    call("close_tab", tab_id)
                self.assertEqual(
                    input("Did the visible browser flicker, move or briefly take focus? [yes/no]: ").strip().lower(),
                    "no", "physical flicker/focus observation failed or was not confirmed",
                )
            finally:
                worker.shutdown(timeout=2)


@unittest.skipUnless(
    os.environ.get("OCTET_BROWSE_PLAYWRIGHT_TESTS") == "1",
    "set OCTET_BROWSE_PLAYWRIGHT_TESTS=1 for local headful snapshot integration",
)
class PlaywrightSnapshotIntegrationTests(unittest.TestCase):
    def test_local_dom_budgets_visibility_editables_unicode_and_redaction(self) -> None:
        # Unlike the navigation suite this test needs no HTTP server or network:
        # all fixtures enter a temporary isolated profile via local set_content.
        runtime_paths = BrowsePaths.for_home()
        setup = SetupManager(runtime_paths)
        try:
            setup.validate_runtime()
        except BrowseError as error:
            self.skipTest(f"pinned runtime unavailable ({error.code})")
        with tempfile.TemporaryDirectory() as home:
            engine = BrowserEngine(
                runtime_paths, setup, ProfileManager(BrowsePaths.for_home(Path(home)))
            )
            owner = ResourceOwner("snapshot-session", "snapshot-instance", 1)
            try:
                launch = engine.launch(OperationContext(time.monotonic() + 25), owner)
                tab = engine._tabs[launch["selected_tab_id"]]
                tab.page.set_content('''<body><p>Hello <span>world 🌍</span><br>下一行</p>
                    <div hidden>HIDDEN_PRIVATE</div>
                    <div style="display:none">DISPLAY_PRIVATE</div>
                    <div style="visibility:hidden">VISIBILITY_PRIVATE</div>
                    <details><summary>Public summary</summary>CLOSED_PRIVATE</details>
                    <script type="application/json">SCRIPT_PRIVATE</script>
                    <style>/* STYLE_PRIVATE */</style>
                    <input type="password" aria-label="Password" value="INPUT_PRIVATE">
                    <textarea hidden>TEXTAREA_PRIVATE</textarea></body>''')
                result = snapshot_page(tab)
                self.assertIn("Hello world 🌍\n下一行", result.text)
                self.assertIn("manual credential field", result.text)
                self.assertIn("Public summary", result.text)
                self.assertNotIn("_PRIVATE", result.text)
                self.assertFalse(result.truncated)

                tab.remember_typed_value("secret value")
                for separator in ("   ", "\t", "\n"):
                    tab.page.set_content(
                        '<body><p style="white-space:normal">Public: secret'
                        + separator + 'value.</p></body>'
                    )
                    # A tiny controlled fixture documents the old innerText
                    # observation; production body extraction never uses it.
                    self.assertEqual(tab.page.locator("p").inner_text(), "Public: secret value.")
                    result = snapshot_page(tab)
                    self.assertIn("Public: [typed value withheld].", result.text)
                    self.assertNotIn("secret", result.text)
                tab.page.set_content(
                    "<body>" + " " * (MAX_BODY_SOURCE_CHARS - len("secret   va"))
                    + "secret   value</body>"
                )
                result = snapshot_page(tab)
                self.assertTrue(result.truncated)
                self.assertIn("source budget exceeded", result.text)
                self.assertIn("Visible text:\n[typed value withheld]", result.text)
                self.assertNotIn("secret", result.text)

                for value in ("abcde", "cdefgh"):
                    tab.remember_typed_value(value)
                tab.page.set_content(
                    "<body>" + " " * (MAX_BODY_SOURCE_CHARS - 5) + "abcdefgh</body>"
                )
                result = snapshot_page(tab)
                self.assertTrue(result.truncated)
                self.assertIn("source budget exceeded", result.text)
                self.assertIn("[typed value withheld]", result.text)
                self.assertNotIn("abc", result.text)
                self.assertLess(len(result.text), 1000)

                tab.page.set_content("<body>" + "<span></span>" * (MAX_BODY_TRAVERSAL_NODES + 1) + "</body>")
                result = snapshot_page(tab)
                self.assertTrue(result.truncated)
                self.assertIn("traversal budget exceeded", result.text)
                for editable in ('<textarea>MANUAL_PRIVATE</textarea>', '<div contenteditable>MANUAL_PRIVATE</div>'):
                    tab.page.set_content("<body>Public text" + editable + "</body>")
                    result = snapshot_page(tab)
                    self.assertIn("editable content could contain manually entered values", result.text)
                    self.assertNotIn("MANUAL_PRIVATE", result.text)
                    self.assertNotIn("Public text", result.text)
            finally:
                engine.shutdown()


if __name__ == "__main__":
    unittest.main()

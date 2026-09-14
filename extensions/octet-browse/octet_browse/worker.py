"""Single-owner Playwright worker and headful persistent browser engine."""

from __future__ import annotations

import importlib
import os
import queue
import secrets
import stat
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional, Tuple
from urllib.parse import urljoin

from .paths import BrowsePaths
from .profile import ProfileLease, ProfileManager
from .safety import (
    BrowseError,
    ResourceOwner,
    bounded_text,
    sanitize_url,
    url_origin,
    valid_tab_id,
    validate_http_url,
)
from .setup import SetupManager
from .snapshot import SnapshotResult, TabState, snapshot_page
from .targeting import TargetMetadata, inspect_target, resolve_target


DEFAULT_OPERATION_TIMEOUT = 12.0
NAVIGATION_TIMEOUT = 15.0
CONFIRMATION_OPERATION_TIMEOUT = 25.0
POPUP_NAVIGATION_GRACE_SECONDS = 1.0
MAX_WORK_QUEUE = 32
MAX_TABS = 32
KEY_ALLOWLIST = (
    "Enter",
    "Tab",
    "Shift+Tab",
    "Escape",
    "Backspace",
    "Delete",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Space",
)


@dataclass
class OperationContext:
    deadline: float
    cancellation: Any = None
    abandoned: threading.Event = field(default_factory=threading.Event)

    def check(self) -> None:
        if self.abandoned.is_set() or time.monotonic() >= self.deadline:
            raise BrowseError(
                "operation_timeout",
                "The bounded browser operation timed out; its outcome may be ambiguous, so do not retry it without inspecting fresh state.",
            )
        token = self.cancellation
        if token is not None:
            try:
                token.raise_if_cancelled()
            except Exception:
                self.abandoned.set()
                raise

    def remaining_ms(self, maximum: int = 15_000) -> int:
        self.check()
        remaining = max(1, int((self.deadline - time.monotonic()) * 1000))
        return min(maximum, remaining)


@dataclass
class _Task:
    method: str
    arguments: Tuple[Any, ...]
    keyword_arguments: Dict[str, Any]
    operation: OperationContext
    done: threading.Event = field(default_factory=threading.Event)
    result: Any = None
    error: Optional[BaseException] = None


class PlaywrightWorker:
    """Serialize every browser-library call on one dedicated owner thread."""

    _STOP = object()

    def __init__(self, engine_factory: Callable[[], Any], *, capacity: int = MAX_WORK_QUEUE) -> None:
        if capacity < 1:
            raise ValueError("worker capacity must be positive")
        self._factory = engine_factory
        self._queue: queue.Queue[Any] = queue.Queue(maxsize=capacity)
        self._closed = threading.Event()
        self._thread = threading.Thread(
            target=self._run,
            name="octet-browse-playwright-owner",
            daemon=True,
        )
        self._thread.start()

    def call(
        self,
        method: str,
        *arguments: Any,
        timeout: float = DEFAULT_OPERATION_TIMEOUT,
        cancellation: Any = None,
        **keyword_arguments: Any,
    ) -> Any:
        if timeout <= 0:
            raise ValueError("worker timeout must be positive")
        if self._closed.is_set():
            raise BrowseError("browser_stopped", "The browser owner worker is stopped.")
        operation = OperationContext(time.monotonic() + timeout, cancellation)
        task = _Task(method, arguments, keyword_arguments, operation)
        while True:
            operation.check()
            try:
                self._queue.put(task, timeout=min(0.05, max(0.001, timeout)))
                break
            except queue.Full:
                continue
        while not task.done.wait(0.05):
            try:
                operation.check()
            except BaseException:
                operation.abandoned.set()
                raise
            if self._closed.is_set():
                operation.abandoned.set()
                raise BrowseError("browser_stopped", "The browser owner worker stopped.")
        if task.error is not None:
            raise task.error
        return task.result

    def shutdown(self, timeout: float = 1.5) -> None:
        if self._closed.is_set():
            return
        deadline = time.monotonic() + max(0.0, timeout)
        while True:
            try:
                self._queue.put(self._STOP, timeout=0.05)
                break
            except queue.Full:
                if time.monotonic() >= deadline:
                    self._closed.set()
                    return
        self._thread.join(timeout=max(0.0, deadline - time.monotonic()))
        self._closed.set()

    def _run(self) -> None:
        engine: Any = None
        try:
            engine = self._factory()
            while True:
                task = self._queue.get()
                if task is self._STOP:
                    break
                if not isinstance(task, _Task):
                    continue
                try:
                    task.operation.check()
                    handler = getattr(engine, task.method)
                    task.result = handler(
                        task.operation, *task.arguments, **task.keyword_arguments
                    )
                except BaseException as error:
                    task.error = error
                finally:
                    task.done.set()
        finally:
            if engine is not None:
                try:
                    engine.shutdown()
                except Exception:
                    pass
            self._closed.set()
            while True:
                try:
                    pending = self._queue.get_nowait()
                except queue.Empty:
                    break
                if isinstance(pending, _Task):
                    pending.error = BrowseError(
                        "browser_stopped", "The browser owner worker stopped."
                    )
                    pending.done.set()


class BrowserEngine:
    """All methods are invoked only on :class:`PlaywrightWorker`'s thread."""

    def __init__(
        self,
        paths: BrowsePaths,
        setup: SetupManager,
        profiles: ProfileManager,
        *,
        tab_id_factory: Optional[Callable[[], str]] = None,
    ) -> None:
        self.paths = paths
        self.setup = setup
        self.profiles = profiles
        self._tab_id_factory = tab_id_factory or (lambda: "tab_" + secrets.token_hex(8))
        self._playwright: Any = None
        self._context: Any = None
        self._context_closed = False
        self._closing_context = False
        self._profile_lease: Optional[ProfileLease] = None
        self._owner: Optional[Tuple[str, str, int]] = None
        self._tabs: Dict[str, TabState] = {}
        self._page_ids: Dict[int, str] = {}
        self._allowed_blank_pages: set[int] = set()
        self._selected_tab_id: Optional[str] = None
        self._download_events = 0
        self._blocked_navigation = False
        self._degraded = False

    def status(self, operation: OperationContext, owner: Optional[ResourceOwner]) -> Dict[str, Any]:
        operation.check()
        self._sync_pages()
        open_browser = self._context is not None
        owner_matches = not open_browser or (
            owner is not None and self._owner == owner.key
        )
        tabs = self._tab_infos() if owner_matches else []
        selected = self._selected_tab_id if owner_matches else None
        selected_origin = "unavailable"
        if selected in self._tabs:
            selected_tab = self._tabs[selected]
            selected_origin = bounded_text(
                selected_tab.redact(url_origin(selected_tab.last_url)), 256
            )
        return {
            "open": open_browser,
            "owner_matches": owner_matches,
            "tab_count": len(tabs),
            "selected_tab_id": selected,
            "selected_origin": selected_origin,
            "tabs": tabs,
            "degraded": self._degraded,
        }

    def launch(self, operation: OperationContext, owner: ResourceOwner) -> Dict[str, Any]:
        operation.check()
        if self._context is not None:
            self._require_owner(owner)
            self._sync_pages()
            # A close event detaches the dead context. Only this explicit launch
            # request may recover it; ordinary browser actions never relaunch or
            # reactivate a browser that the user closed or that crashed.
            if self._context is not None:
                self._degraded = False
                created_tab_id: Optional[str] = None
                created_page: Any = None
                try:
                    if not self._tabs:
                        created_page = self._context.new_page()
                        operation.check()
                        created_tab_id = self._register_page(created_page).tab_id
                    operation.check()
                    text = "Visible isolated browser already open."
                    if created_tab_id is not None:
                        text += f" Created tab {created_tab_id}."
                    elif self._selected_tab_id is not None:
                        text += f" Selected tab {self._selected_tab_id}."
                    result = self._browser_result(text)
                    operation.check()
                    if not result.get("open"):
                        raise BrowseError(
                            "browser_degraded",
                            "The browser context ended during launch; inspect status and relaunch it.",
                        )
                    result["created_tab_ids"] = [created_tab_id] if created_tab_id else []
                    return result
                except BaseException:
                    # A cancelled repeated-open must not leave a newly created
                    # blank tab visible after its caller has gone away.
                    if created_page is not None:
                        try:
                            created_page.close()
                        except Exception:
                            pass
                        if created_tab_id is not None:
                            self._remove_tab(created_tab_id)
                    raise

        self.setup.validate_runtime()
        lease = self.profiles.acquire(create=True)
        playwright = None
        context = None
        context_admitted = False
        try:
            operation.check()
            sync_api = self._load_pinned_playwright()
            os.environ["PLAYWRIGHT_BROWSERS_PATH"] = str(self.paths.runtime / "browsers")
            playwright = sync_api.sync_playwright().start()
            operation.check()
            executable = Path(playwright.chromium.executable_path)
            self._validate_browser_executable(executable)
            operation.check()
            context = playwright.chromium.launch_persistent_context(
                user_data_dir=str(lease.path),
                executable_path=str(executable),
                headless=False,
                accept_downloads=False,
                viewport={"width": 1280, "height": 800},
                timeout=operation.remaining_ms(),
            )
            # A timed-out/cancelled launch may return after the caller has
            # abandoned its request. Check before admitting the visible context
            # so the exception path closes it instead of leaving a stray browser
            # that can later reacquire the foreground.
            operation.check()
            context.set_default_timeout(5000)
            context.set_default_navigation_timeout(int(NAVIGATION_TIMEOUT * 1000))
            context.route("**/*", self._route_request)
            context.on("page", self._handle_new_page)
            self._playwright = playwright
            self._context = context
            self._context_closed = False
            self._profile_lease = lease
            context_admitted = True
            self._watch_context(context)
            self._owner = owner.key
            self._degraded = False
            for page in list(context.pages):
                self._register_page(page)
            if not self._tabs:
                self._register_page(context.new_page())
            self._sync_pages()
            operation.check()
            if self._context is None:
                raise BrowseError(
                    "browser_degraded",
                    "The browser context ended during launch; inspect status and relaunch it.",
                )
            tab_ids = sorted(self._tabs)
            text = "Opened visible isolated browser."
            if tab_ids:
                text += " Tabs: " + ", ".join(tab_ids) + "."
            result = self._browser_result(text)
            operation.check()
            if not result.get("open"):
                raise BrowseError(
                    "browser_degraded",
                    "The browser context ended during launch; inspect status and relaunch it.",
                )
            result["created_tab_ids"] = tab_ids
            return result
        except BaseException as error:
            self._degraded = True
            if self._context is not None:
                self._close_browser(preserve_degraded=True)
            elif not context_admitted:
                if context is not None:
                    try:
                        context.close()
                    except Exception:
                        pass
                if playwright is not None:
                    try:
                        playwright.stop()
                    except Exception:
                        pass
                lease.release()
            if isinstance(error, BrowseError):
                raise
            raise BrowseError(
                "launch_failed",
                "The visible isolated Chromium browser could not be launched; check /browse status.",
            ) from error

    def tabs(self, operation: OperationContext, owner: ResourceOwner) -> Dict[str, Any]:
        self._require_open(owner)
        operation.check()
        self._sync_pages()
        return self._browser_result("Listed explicit browser tabs.")

    def open_url(
        self,
        operation: OperationContext,
        owner: ResourceOwner,
        url: str,
        tab_id: Optional[str],
    ) -> Dict[str, Any]:
        self._require_open(owner)
        normalized = validate_http_url(url)
        created = False
        if tab_id is None:
            operation.check()
            page = self._context.new_page()
            tab = self._register_page(page)
            created = True
        else:
            tab = self._require_tab(tab_id)
            page = tab.page
        self._selected_tab_id = tab.tab_id
        tab.invalidate()
        self._blocked_navigation = False
        try:
            page.goto(
                normalized,
                wait_until="domcontentloaded",
                timeout=operation.remaining_ms(int(NAVIGATION_TIMEOUT * 1000)),
            )
            operation.check()
            self._sync_pages()
        except BaseException as error:
            self._sync_pages()
            if isinstance(error, BrowseError):
                raise
            if self._blocked_navigation:
                raise BrowseError(
                    "navigation_blocked",
                    f"Blocked an unsafe redirect or top-level navigation in tab {tab.tab_id}.",
                ) from error
            raise BrowseError(
                "navigation_failed", f"Navigation did not complete in tab {tab.tab_id}."
            ) from error
        safe_origin = bounded_text(tab.redact(url_origin(page.url)), 256)
        result = self._browser_result(
            ("Created and navigated" if created else "Navigated")
            + f" tab {tab.tab_id} to allowed origin {safe_origin}."
        )
        result["affected_tab_id"] = tab.tab_id
        result["created_tab_ids"] = [tab.tab_id] if created else []
        return result

    def snapshot(
        self, operation: OperationContext, owner: ResourceOwner, tab_id: str
    ) -> Dict[str, Any]:
        self._require_open(owner)
        tab = self._require_tab(tab_id)
        self._selected_tab_id = tab_id
        operation.check()
        result: SnapshotResult = snapshot_page(tab)
        return {
            **self._browser_result(result.text),
            "affected_tab_id": tab_id,
            "snapshot_generation": result.generation,
            "element_count": result.element_count,
            "truncated": result.truncated,
        }

    def click(
        self,
        operation: OperationContext,
        owner: ResourceOwner,
        tab_id: str,
        target: str,
        snapshot_generation: Any,
        confirmation: Callable[[str, bool, str], bool],
    ) -> Dict[str, Any]:
        self._require_open(owner)
        tab = self._require_tab(tab_id)
        self._selected_tab_id = tab_id
        resolved = resolve_target(tab.page, tab, target, snapshot_generation)
        self._validate_target_navigation(tab.page, resolved.metadata)
        if resolved.metadata.consequential:
            category = _consequence_category(resolved.metadata)
            operation.check()
            confirmed = confirmation(category, resolved.metadata.destructive, url_origin(tab.page.url))
            operation.check()
            if not confirmed:
                raise BrowseError(
                    "confirmation_denied",
                    "The consequential browser action was denied or no interactive confirmation was available.",
                )
        before_ids = set(self._tabs)
        before_downloads = self._download_events
        self._blocked_navigation = False
        try:
            if resolved.metadata.opens_popup and callable(getattr(tab.page, "expect_popup", None)):
                with tab.page.expect_popup(timeout=operation.remaining_ms()) as popup_info:
                    resolved.target.click(timeout=operation.remaining_ms())
                self._register_page(popup_info.value)
            else:
                resolved.target.click(timeout=operation.remaining_ms())
            operation.check()
            self._settle_new_pages(operation, before_ids)
            self._sync_pages()
            if self._context is None:
                raise BrowseError(
                    "browser_degraded",
                    "The browser context ended during the click; inspect status and relaunch it.",
                )
            if self._blocked_navigation:
                raise BrowseError(
                    "navigation_blocked",
                    f"Blocked an unsafe link, redirect, or popup from tab {tab_id}.",
                )
        except BaseException as error:
            self._sync_pages()
            if isinstance(error, BrowseError):
                raise
            if self._blocked_navigation:
                raise BrowseError(
                    "navigation_blocked",
                    f"Blocked an unsafe link, redirect, or popup from tab {tab_id}.",
                ) from error
            raise BrowseError("click_failed", f"The bounded click failed in tab {tab_id}.") from error
        after_ids = set(self._tabs)
        created = sorted(after_ids - before_ids)
        closed = sorted(before_ids - after_ids)
        download_blocked = self._download_events > before_downloads
        pieces = [f"Clicked the unique target in tab {tab_id}."]
        if created:
            pieces.append("Popup tabs created: " + ", ".join(created) + ".")
        if closed:
            pieces.append("Tabs closed: " + ", ".join(closed) + ".")
        if download_blocked:
            pieces.append("A download was blocked.")
        return {
            **self._browser_result(" ".join(pieces)),
            "affected_tab_id": tab_id,
            "created_tab_ids": created,
            "closed_tab_ids": closed,
            "download_blocked": download_blocked,
        }

    def type_text(
        self,
        operation: OperationContext,
        owner: ResourceOwner,
        tab_id: str,
        target: str,
        snapshot_generation: Any,
        value: str,
    ) -> Dict[str, Any]:
        self._require_open(owner)
        if not isinstance(value, str) or len(value) > 4096 or len(value.encode("utf-8")) > 16_384:
            raise BrowseError("invalid_arguments", "Typed text exceeds the bounded input limit.")
        tab = self._require_tab(tab_id)
        self._selected_tab_id = tab_id
        resolved = resolve_target(tab.page, tab, target, snapshot_generation)
        if resolved.metadata.credential_like:
            raise BrowseError(
                "manual_auth_required",
                "Typing into password, OTP, payment, authentication, or credential-like fields is disabled; enter credentials manually in the visible browser.",
            )
        if not resolved.metadata.fillable:
            raise BrowseError(
                "target_not_typeable",
                "browser_type accepts only native non-credential input or textarea fields.",
            )
        try:
            operation.check()
            resolved.target.fill(value, timeout=operation.remaining_ms())
            operation.check()
            tab.remember_typed_value(value)
        except BaseException as error:
            if isinstance(error, BrowseError):
                raise
            # Never include Playwright's message here: browser-controlled error
            # text can contain the typed value or page labels.
            raise BrowseError(
                "type_failed",
                f"Typing failed in tab {tab_id}; the supplied value was withheld.",
            ) from error
        return {
            **self._browser_result(
                f"Typed into the unique non-credential field in tab {tab_id}; value withheld."
            ),
            "affected_tab_id": tab_id,
            "value_echoed": False,
        }

    def press(
        self,
        operation: OperationContext,
        owner: ResourceOwner,
        tab_id: str,
        target: str,
        snapshot_generation: Any,
        key: str,
        confirmation: Callable[[str, bool, str], bool],
    ) -> Dict[str, Any]:
        self._require_open(owner)
        if key not in KEY_ALLOWLIST:
            raise BrowseError(
                "key_not_allowed",
                "The key is outside the documented navigation-key allowlist; clipboard shortcuts are disabled.",
            )
        tab = self._require_tab(tab_id)
        self._selected_tab_id = tab_id
        resolved = resolve_target(tab.page, tab, target, snapshot_generation)
        consequential = resolved.metadata.consequential or (key == "Enter" and resolved.metadata.in_form)
        if consequential and key in {"Enter", "Space"}:
            category = _consequence_category(resolved.metadata)
            operation.check()
            if not confirmation(category, resolved.metadata.destructive, url_origin(tab.page.url)):
                raise BrowseError(
                    "confirmation_denied",
                    "The consequential key action was denied or no interactive confirmation was available.",
                )
            operation.check()
        before_downloads = self._download_events
        self._blocked_navigation = False
        try:
            resolved.target.press(key, timeout=operation.remaining_ms())
            operation.check()
            self._sync_pages()
            if self._blocked_navigation:
                raise BrowseError(
                    "navigation_blocked",
                    f"Blocked an unsafe key-triggered navigation in tab {tab_id}.",
                )
        except BaseException as error:
            self._sync_pages()
            if isinstance(error, BrowseError):
                raise
            if self._blocked_navigation:
                raise BrowseError(
                    "navigation_blocked", f"Blocked an unsafe key-triggered navigation in tab {tab_id}."
                ) from error
            raise BrowseError("press_failed", f"The bounded key action failed in tab {tab_id}.") from error
        blocked = self._download_events > before_downloads
        text = f"Pressed allowed key {key} on the unique target in tab {tab_id}."
        if blocked:
            text += " A download was blocked."
        return {
            **self._browser_result(text),
            "affected_tab_id": tab_id,
            "download_blocked": blocked,
        }

    def scroll(
        self,
        operation: OperationContext,
        owner: ResourceOwner,
        tab_id: str,
        delta_x: int,
        delta_y: int,
    ) -> Dict[str, Any]:
        self._require_open(owner)
        tab = self._require_tab(tab_id)
        self._selected_tab_id = tab_id
        try:
            operation.check()
            tab.page.mouse.wheel(delta_x, delta_y)
            operation.check()
        except BaseException as error:
            if isinstance(error, BrowseError):
                raise
            raise BrowseError("scroll_failed", f"The bounded scroll failed in tab {tab_id}.") from error
        return {
            **self._browser_result(f"Scrolled tab {tab_id} by a bounded distance."),
            "affected_tab_id": tab_id,
        }

    def wait(
        self,
        operation: OperationContext,
        owner: ResourceOwner,
        tab_id: str,
        milliseconds: int,
    ) -> Dict[str, Any]:
        self._require_open(owner)
        tab = self._require_tab(tab_id)
        self._selected_tab_id = tab_id
        try:
            operation.check()
            tab.page.wait_for_timeout(milliseconds)
            operation.check()
            self._sync_pages()
        except BaseException as error:
            if isinstance(error, BrowseError):
                raise
            raise BrowseError("wait_failed", f"The bounded wait failed in tab {tab_id}.") from error
        return {
            **self._browser_result(f"Waited {milliseconds} ms in tab {tab_id}."),
            "affected_tab_id": tab_id,
        }

    def screenshot(
        self, operation: OperationContext, owner: ResourceOwner, tab_id: str
    ) -> Dict[str, Any]:
        self._require_open(owner)
        tab = self._require_tab(tab_id)
        self._selected_tab_id = tab_id
        self._require_screenshot_safe(tab)
        try:
            operation.check()
            data = tab.page.screenshot(
                type="png",
                full_page=False,
                animations="disabled",
                timeout=operation.remaining_ms(),
            )
            operation.check()
        except BaseException as error:
            if isinstance(error, BrowseError):
                raise
            raise BrowseError(
                "screenshot_failed", f"The viewport screenshot failed in tab {tab_id}."
            ) from error
        if not isinstance(data, bytes):
            data = bytes(data)
        return {"affected_tab_id": tab_id, "data": data, **self._browser_result("")}

    def close_tab(
        self, operation: OperationContext, owner: ResourceOwner, tab_id: str
    ) -> Dict[str, Any]:
        self._require_open(owner)
        tab = self._require_tab(tab_id)
        operation.check()
        tab.close_references()
        try:
            tab.page.close()
            operation.check()
        except BaseException as error:
            if isinstance(error, BrowseError):
                raise
            raise BrowseError("tab_close_failed", f"Tab {tab_id} could not be closed.") from error
        self._remove_tab(tab_id)
        self._sync_pages()
        return {
            **self._browser_result(f"Closed tab {tab_id}."),
            "affected_tab_id": tab_id,
            "closed_tab_ids": [tab_id],
        }

    def close(self, operation: OperationContext, owner: ResourceOwner) -> Dict[str, Any]:
        operation.check()
        self._sync_pages()
        if self._context is None:
            return self._browser_result("Browser already closed; no tab IDs were affected.")
        self._require_owner(owner)
        tab_ids = sorted(self._tabs)
        self._close_browser()
        text = "Closed the visible isolated browser context."
        if tab_ids:
            text += " Closed tabs: " + ", ".join(tab_ids) + "."
        return {
            **self._browser_result(text),
            "closed_tab_ids": tab_ids,
        }

    def force_close(self, operation: OperationContext) -> Dict[str, Any]:
        operation.check()
        tab_ids = sorted(self._tabs)
        self._close_browser()
        text = "Closed the visible isolated browser context."
        if tab_ids:
            text += " Closed tabs: " + ", ".join(tab_ids) + "."
        return {
            **self._browser_result(text),
            "closed_tab_ids": tab_ids,
        }

    def shutdown(self) -> None:
        self._close_browser()

    def _load_pinned_playwright(self) -> Any:
        site = self.setup.site_packages().absolute()
        site_resolved = site.resolve()
        existing = sys.modules.get("playwright")
        if existing is not None:
            module_path = Path(str(getattr(existing, "__file__", ""))).resolve()
            if site_resolved not in module_path.parents:
                raise BrowseError(
                    "ambient_playwright_refused",
                    "An ambient Playwright package was refused; restart octet and use /browse setup.",
                )
        site_text = str(site)
        if site_text not in sys.path:
            sys.path.insert(0, site_text)
        importlib.invalidate_caches()
        try:
            module = importlib.import_module("playwright.sync_api")
        except Exception as error:
            raise BrowseError("runtime_invalid", "The pinned Playwright package could not be loaded.") from error
        module_path = Path(str(getattr(module, "__file__", ""))).resolve()
        if site_resolved not in module_path.parents:
            raise BrowseError("ambient_playwright_refused", "An ambient Playwright package was refused.")
        return module

    def _validate_browser_executable(self, executable: Path) -> None:
        browser_root = (self.paths.runtime / "browsers").resolve()
        try:
            resolved = executable.resolve(strict=True)
            metadata = resolved.lstat()
        except (OSError, RuntimeError) as error:
            raise BrowseError("runtime_invalid", "The isolated Chromium executable is unavailable.") from error
        if browser_root not in resolved.parents or not stat.S_ISREG(metadata.st_mode):
            raise BrowseError(
                "ambient_browser_refused",
                "The browser executable is outside the pinned octet-owned runtime.",
            )

    def _route_request(self, route: Any, request: Any) -> None:
        try:
            is_navigation = bool(request.is_navigation_request())
        except Exception:
            is_navigation = True
        if is_navigation:
            try:
                try:
                    parent_frame = request.frame.parent_frame
                except Exception:
                    # Chromium can issue a popup's first navigation before its
                    # frame is available. Treat the request as top-level and
                    # validate its URL rather than rejecting a valid popup.
                    parent_frame = None
                if parent_frame is None and request.url != "about:blank":
                    validate_http_url(request.url)
            except Exception:
                self._blocked_navigation = True
                try:
                    route.abort("blockedbyclient")
                except Exception:
                    pass
                return
        try:
            route.continue_()
        except Exception:
            # A failed continue is a browser transport failure, not grounds to
            # retry or silently authorize another route.
            pass

    def _watch_context(self, context: Any) -> None:
        """Observe an externally closed context without ever relaunching it."""

        try:
            context.on(
                "close",
                lambda *_arguments: self._handle_context_close(context),
            )
        except Exception:
            # Older/fake context implementations may not expose lifecycle
            # events. The regular pages probe still detects transport failures.
            pass

    def _handle_context_close(self, context: Any) -> None:
        if getattr(self, "_closing_context", False) or context is not self._context:
            return
        self._context_closed = True
        self._degraded = True

    @staticmethod
    def _context_has_ended(context: Any) -> bool:
        try:
            probe = getattr(context, "is_closed", None)
        except Exception:
            return True
        if probe is None:
            return False
        try:
            return bool(probe() if callable(probe) else probe)
        except Exception:
            return True

    def _handle_new_page(self, page: Any) -> None:
        try:
            self._register_page(page)
        except BrowseError:
            # The initiating operation observes _blocked_navigation or the
            # explicit registration failure and returns a bounded error.
            pass

    def _register_page(self, page: Any) -> TabState:
        identity = id(page)
        existing_id = self._page_ids.get(identity)
        if existing_id is not None and existing_id in self._tabs:
            return self._tabs[existing_id]
        if len(self._tabs) >= MAX_TABS:
            self._blocked_navigation = True
            try:
                page.close()
            except Exception:
                pass
            raise BrowseError(
                "tab_limit",
                f"The visible browser is limited to {MAX_TABS} tabs; the additional tab was closed.",
            )
        tab_id = self._new_tab_id()
        tab = TabState(tab_id=tab_id, page=page, last_url=str(getattr(page, "url", "about:blank")))
        self._tabs[tab_id] = tab
        self._page_ids[identity] = tab_id
        try:
            opener = page.opener
            # Playwright's sync API exposes Page.opener as a method; treating
            # the bound method itself as an opener incorrectly drops the normal
            # initial about:blank page during the next lifecycle sync.
            if callable(opener):
                opener = opener()
        except Exception:
            opener = None
        if tab.last_url == "about:blank" and opener is None:
            self._allowed_blank_pages.add(identity)
        self._selected_tab_id = tab_id
        try:
            page.on("download", self._block_download)
        except Exception:
            pass
        return tab

    def _block_download(self, download: Any) -> None:
        self._download_events += 1
        try:
            download.cancel()
        except Exception:
            pass

    def _settle_new_pages(
        self, operation: OperationContext, before_ids: set[str]
    ) -> None:
        """Give popup navigations a bounded chance to leave about:blank.

        Playwright emits a popup before its target navigation settles. Syncing
        immediately after the click would otherwise classify that transient
        about:blank as an unsafe page and close a valid popup.
        """

        context = self._context
        if context is None:
            return
        pages = list(context.pages)
        for page in pages:
            self._register_page(page)
        pending = {
            tab_id
            for tab_id, tab in self._tabs.items()
            if tab_id not in before_ids
            and str(getattr(tab.page, "url", "about:blank")) == "about:blank"
        }
        deadline = min(
            operation.deadline,
            time.monotonic() + POPUP_NAVIGATION_GRACE_SECONDS,
        )
        while pending and time.monotonic() < deadline:
            operation.check()
            for tab_id in list(pending):
                tab = self._tabs.get(tab_id)
                if tab is None or str(getattr(tab.page, "url", "about:blank")) != "about:blank":
                    pending.discard(tab_id)
            if not pending:
                return
            remaining_ms = max(
                1,
                min(50, int((deadline - time.monotonic()) * 1000)),
            )
            tab = self._tabs.get(next(iter(pending)))
            if tab is None:
                continue
            try:
                tab.page.wait_for_timeout(remaining_ms)
            except Exception:
                break
            pages = list(context.pages)
            for page in pages:
                self._register_page(page)

    def _sync_pages(self) -> None:
        context = self._context
        if context is None:
            return
        if getattr(self, "_context_closed", False) or self._context_has_ended(context):
            self._degraded = True
            self._close_browser(preserve_degraded=True)
            return
        try:
            pages = list(context.pages)
            # A close event can be dispatched while the pages list is read.
            # Do not publish the old context as open in that case.
            if getattr(self, "_context_closed", False) or self._context_has_ended(context):
                self._degraded = True
                self._close_browser(preserve_degraded=True)
                return
        except Exception:
            self._degraded = True
            self._close_browser(preserve_degraded=True)
            return
        live = {id(page) for page in pages}
        for page in pages:
            self._register_page(page)
        for tab_id, tab in list(self._tabs.items()):
            closed = id(tab.page) not in live
            if not closed:
                try:
                    closed = bool(tab.page.is_closed())
                except Exception:
                    closed = True
            if closed:
                self._remove_tab(tab_id)
                continue
            current_url = str(getattr(tab.page, "url", "about:blank"))
            if current_url == "about:blank" and id(tab.page) not in self._allowed_blank_pages:
                self._blocked_navigation = True
                try:
                    tab.page.close()
                except Exception:
                    pass
                self._remove_tab(tab_id)
                continue
            if current_url != "about:blank":
                self._allowed_blank_pages.discard(id(tab.page))
                try:
                    validate_http_url(current_url)
                except BrowseError:
                    self._blocked_navigation = True
                    try:
                        tab.page.close()
                    except Exception:
                        pass
                    self._remove_tab(tab_id)
                    continue
            if current_url != tab.last_url:
                tab.invalidate()
                tab.last_url = current_url
            try:
                tab.title = bounded_text(tab.redact(tab.page.title()), 256)
            except Exception:
                tab.title = ""
        if self._selected_tab_id not in self._tabs:
            self._selected_tab_id = next(iter(self._tabs), None)

    def _allow_form_screenshots(self) -> bool:
        """Opt-in override: screenshots tolerate visible form/editable fields.

        Enabled by either the environment variable
        ``OCTET_BROWSE_ALLOW_FORM_SCREENSHOTS`` or the sentinel file
        ``~/.octet/browse/allow-form-screenshots``. Credential-like fields and
        tool-typed values always stay refused.
        """
        value = os.environ.get("OCTET_BROWSE_ALLOW_FORM_SCREENSHOTS", "")
        if value.strip().lower() in {"1", "true", "yes", "on"}:
            return True
        try:
            sentinel = self.paths.root / "allow-form-screenshots"
            return sentinel.is_file() and not sentinel.is_symlink()
        except OSError:
            return False

    def _require_screenshot_safe(self, tab: TabState) -> None:
        if tab.has_typed_values:
            raise BrowseError(
                "screenshot_typed_values",
                "Screenshot refused because this tab contains a value supplied through browser_type.",
            )
        allow_forms = self._allow_form_screenshots()
        try:
            controls = tab.page.locator(
                'css=input:not([type="hidden"]), textarea, [contenteditable="true"]'
            )
            count = min(controls.count(), 100)
            for index in range(count):
                candidate = controls.nth(index)
                if not candidate.is_visible():
                    continue
                if inspect_target(candidate).credential_like:
                    raise BrowseError(
                        "screenshot_manual_auth",
                        "Screenshot refused while a visible credential, OTP, payment, or authentication field is present; inspect the page semantically or finish authentication manually.",
                    )
                if not allow_forms:
                    raise BrowseError(
                        "screenshot_form_values",
                        "Screenshot refused while a visible form or editable field could contain a manually entered value.",
                    )
        except BrowseError:
            raise
        except Exception as error:
            raise BrowseError(
                "screenshot_safety_unknown",
                "Screenshot refused because sensitive-field safety could not be established.",
            ) from error

    def _validate_target_navigation(self, page: Any, metadata: TargetMetadata) -> None:
        for raw in (metadata.href, metadata.form_action):
            if raw is None or not raw.strip():
                continue
            try:
                absolute = urljoin(str(getattr(page, "url", "")), raw)
                validate_http_url(absolute)
            except BrowseError as error:
                raise BrowseError(
                    "navigation_blocked",
                    "The target's top-level navigation is not an allowed HTTP(S) URL.",
                ) from error

    def _require_open(self, owner: ResourceOwner) -> None:
        if self._context is None:
            raise BrowseError("browser_closed", "The visible isolated browser is not open.")
        self._require_owner(owner)
        self._sync_pages()
        if self._context is None:
            raise BrowseError(
                "browser_degraded",
                "The browser context ended unexpectedly; inspect status and relaunch it.",
            )

    def _require_owner(self, owner: ResourceOwner) -> None:
        if self._owner != owner.key:
            raise BrowseError(
                "owner_mismatch",
                "The browser belongs to a different host-derived resource owner; close it from that session first.",
            )

    def _require_tab(self, tab_id: str) -> TabState:
        if not valid_tab_id(tab_id):
            raise BrowseError("invalid_tab", "An opaque tab_id returned by octet Browse is required.")
        self._sync_pages()
        tab = self._tabs.get(tab_id)
        if tab is None:
            raise BrowseError("tab_missing", f"Tab {bounded_text(tab_id, 64)} is closed or unavailable.")
        return tab

    def _remove_tab(self, tab_id: str) -> None:
        tab = self._tabs.pop(tab_id, None)
        if tab is None:
            return
        tab.close_references()
        self._page_ids.pop(id(tab.page), None)
        self._allowed_blank_pages.discard(id(tab.page))
        if self._selected_tab_id == tab_id:
            self._selected_tab_id = next(iter(self._tabs), None)

    def _close_browser(self, *, preserve_degraded: bool = False) -> None:
        context, self._context = self._context, None
        playwright, self._playwright = self._playwright, None
        lease, self._profile_lease = self._profile_lease, None
        self._context_closed = False
        for tab in self._tabs.values():
            tab.close_references()
        self._tabs.clear()
        self._page_ids.clear()
        self._allowed_blank_pages.clear()
        self._selected_tab_id = None
        self._owner = None
        if not preserve_degraded:
            self._degraded = False
        closing_before = getattr(self, "_closing_context", False)
        self._closing_context = True
        try:
            if context is not None:
                try:
                    context.close()
                except Exception:
                    pass
            if playwright is not None:
                try:
                    playwright.stop()
                except Exception:
                    pass
        finally:
            self._closing_context = closing_before
        if lease is not None:
            lease.release()

    def _tab_infos(self) -> List[Dict[str, Any]]:
        result = []
        for tab_id, tab in self._tabs.items():
            result.append(
                {
                    "tab_id": tab_id,
                    "title": bounded_text(tab.redact(tab.title or "Untitled"), 160),
                    "url": bounded_text(tab.redact(sanitize_url(tab.last_url)), 512),
                    "origin": bounded_text(tab.redact(url_origin(tab.last_url)), 256),
                    "snapshot_generation": tab.generation if tab.references else None,
                    "selected": tab_id == self._selected_tab_id,
                }
            )
        return result

    def _browser_result(self, text: str) -> Dict[str, Any]:
        self._sync_pages()
        return {
            "text": text,
            "open": self._context is not None,
            "tabs": self._tab_infos(),
            "tab_count": len(self._tabs),
            "selected_tab_id": self._selected_tab_id,
            "degraded": self._degraded,
        }

    def _new_tab_id(self) -> str:
        for _ in range(32):
            value = self._tab_id_factory()
            if valid_tab_id(value) and value not in self._tabs:
                return value
        raise BrowseError("tab_id_failed", "A unique opaque tab ID could not be allocated.")


def _consequence_category(metadata: TargetMetadata) -> str:
    value = metadata.name.lower()
    if any(term in value for term in ("delete", "remove", "erase", "unsubscribe")):
        return "delete external data"
    if any(term in value for term in ("buy", "purchase", "pay", "order", "checkout", "transfer")):
        return "submit a purchase or payment"
    if any(term in value for term in ("send", "publish", "post")):
        return "send or publish content"
    if any(term in value for term in ("grant", "authorize", "consent", "accept", "agree")):
        return "grant consent or authorization"
    return "submit an external side effect"

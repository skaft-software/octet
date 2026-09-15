# Browse execution — #377 and #378

## Scope and baseline

- Base: `df5a7e809715961b9344af6b52e43a6ca48f56b3` (`df5a7e80`) plus this worker's eventual working diff. No commits or branch/index manipulation.
- Exclusive edits: `extensions/octet-browse/**` and this file. Other worktree changes are shared and not reverted.
- Read the complete Browse README, REFERENCE, CONNECTORS, CHANGELOG, skill, release 0.7.6 record and installation guide; inspected the worker lifecycle, connector implementation, default worker tests and full opt-in integration test. No Browse-specific qualification record was found under `docs/qualification`; REFERENCE's test section explicitly distinguishes fixtures from live-browser qualification.
- No browser launched, live-browser automation run, setup download requested, normal profile inspected, or native focus/permission API invoked. Physical TUI focus remains **UNRUN**.

## Investigation in progress

### #377

- Base `worker.py:501-508` launches headful Chromium without activation suppression. Repeated open reuses the context; timeout/cancellation closes an unadmitted browser. Those fixes do not address activation by Chromium itself.
- Read-only inspection of the already-installed pinned Playwright 1.57.0 driver found `crBrowser.js:317` creates a target without `background: true`. Installed `browsers.json` identifies Chromium revision 1200 / 143.0.7499.4. Inspecting that runtime's executable strings found `test-type` and `no-startup-window`, but **not** `no-activate`; inventing an inert launch switch is not an acceptable fix.
- Investigating supported non-activating behavior while preserving a visible isolated browser. Headless, offscreen/minimized windows, normal profiles, and post-action foreground stealing back to the TUI are excluded.

### #378

- `adapters.py` supports only Chromium-family Playwright pages. Native Firefox/Safari classes are unsupported descriptors. `PlaywrightTarget` requires a Playwright-compatible page and an exact connector/browser/session/window/tab identity proof; `BrowserEngine` actions and policy interception use that page API.
- No native connector implementation, pinned native transport, trusted native target picker, or host-issued native target identity bridge exists in Browse. Merely changing `supported` to allow Firefox/Safari would falsely claim native support and bypass the current fail-closed declaration.

## Commands/results so far

- `git rev-parse HEAD`: base above; initial `git status --short`: clean at observation.
- Scoped `rg`, file reads and `find`: located implementation/docs/tests and existing pinned runtime (read only).
- One qualification search named nonexistent `docs/qualification/README.md`; `rg` reported the missing path. No Browse qualification file was located in the actual directory.
- At the initial checkpoint no tests had run and no implementation had changed.

## Implementation checkpoint

- Candidate fixes isolated tool-created tab activation by sending only the fixed internal `Target.createTarget` request with `background: true`; it matches the returned target ID, waits with the operation budget, cleans up exact cancelled targets, and has no foreground fallback. Owner, URL and tab-limit checks precede creation. Visible persistent launch and its default arguments remain intact.
- **#377 remains partial**: initial explicit launch and page-driven popup/focus behavior are not suppressed. Chromium's source supports background CDP target creation; no supported general no-activate launch switch was established. `--no-startup-window` would suppress the startup window and stall the pinned default-context page wait; bypassing all Playwright defaults is not a safe complete replacement.
- Added dependency-free background-tab and native-connector boundary regressions, extended the existing opt-in local HTTP integration test to create a new non-activating tab, and added a separately opted-in interactive macOS foreground-PID test. The latter asks the user to refocus the terminal after first launch and confirm no transient flicker; it is explicitly not an initial-launch or actual-TUI pass. No live tests/probes were run.
- Added `extensions/octet-browse/QUALIFICATION.md` with pinned-source evidence, precise scope and remaining physical gates; README/reference state the limitation instead of promising global focus preservation.

### Review: persistent `context.browser` and event pumping

The pinned **installed** Playwright 1.57.0 implementation was inspected, not inferred from the fakes:

- `playwright/driver/package/lib/server/dispatchers/browserTypeDispatcher.js:42-48` constructs a `BrowserDispatcher` for `launchPersistentContext`, parents `BrowserContextDispatcher` to it, and returns both `browser` and `context`.
- `playwright/_impl/_browser_type.py:161-184` decodes both objects and returns that context; `_impl/_browser_context.py:106-114` stores a parent whose class name is `Browser` in `_browser`; its property at `:299-301` returns it. The old-version persistent-context `browser=None` limitation does not describe this pinned path.
- Executed the installed runtime's `venv/bin/python` with an offline Python heredoc that instantiated the **actual installed** `BrowserType`, `Browser`, `BrowserContext` classes using dispatcher-shaped parentage, substituted only `send_return_as_dict` with an `AsyncMock` returning their channels, then awaited the actual `launch_persistent_context` binding. Assertions: version is 1.57.0, returned context identity matches, `returned.browser is browser`, `new_browser_cdp_session` is callable, request is `launchPersistentContext`, and the placeholder profile path is absent before/after. Final attempt **PASS**, exit 0; no transport/browser/profile was started. Two earlier fixture attempts failed because the minimal fake Playwright selector object lacked `_playwright`, then `_test_id_attribute_name`; corrected the fixture after reading parameter preparation. These are not product failures or live qualification.
- The candidate loop excludes already-seen page identities before deciding to wait, so an unmatched popup cannot remain a busy-loop candidate. `wait_for_event("page", timeout=operation.remaining_ms())` pumps Playwright events. Tests cover delayed delivery and target arrival during an unrelated page's probe; adding explicit unmatched-before-delayed and missing-browser regressions in response to review.

### Tests executed so far

1. `cd extensions/octet-browse && env -u OCTET_BROWSE_PLAYWRIGHT_TESTS python3 -m unittest tests.test_background_tabs tests.test_worker -v`: 27 tests, 1 failure in the new assertion expecting `navigation_blocked` for an explicit invalid URL. Existing boundary correctly reports `invalid_url`; corrected the test expectation, not product behavior.
2. `cd extensions/octet-browse && env -u OCTET_BROWSE_PLAYWRIGHT_TESTS -u OCTET_BROWSE_FOCUS_TESTS python3 -m unittest tests.test_background_tabs tests.test_adapters tests.test_worker -v`: **32 passed**, exit 0.
3. Complete suite, compile check and final diff inspection were still pending at that checkpoint; final results follow.

## Final verification and disposition

- `cd extensions/octet-browse && env -u OCTET_BROWSE_PLAYWRIGHT_TESTS -u OCTET_BROWSE_FOCUS_TESTS python3 -m unittest discover -s tests -t . -v`: **82 tests, 80 passed, 2 explicitly skipped**, exit 0 (1.796 s reported by unittest). Both skips are the live HTTP/browser journey and interactive physical-focus test. Default package/protocol/safety/profile/setup/snapshot/artifact/worker tests all passed, including the local packaging smoke and vendored SDK byte-identity guard. No live setup/download/browser was run.
- `python3 -m compileall -q extensions/octet-browse`: exit 0.
- `git diff --check -- extensions/octet-browse docs/swarm-audit/EXECUTION-browse.md`: exit 0. Reviewed the scoped tracked diff plus the new test/docs contents; no production files outside the assigned Browse subtree were changed. Existing/shared unrelated changes were not touched.
- Review-requested regressions now cover `context.browser=None` fail-safe (closed/degraded, no fallback), initial empty-context creation, unmatched popup before delayed matching target (one bounded event wait), and matching target arrival while an unrelated page is being probed. Installed pinned-runtime evidence above confirms persistent-context Browser availability; older versions are not supported or shimmed.
- **#377: partial candidate, NOT closed.** Implemented root-cause repair for tool-created tabs and added a physical post-launch acceptance harness. Remaining: supported non-activating initial launch/page-popup handling and user-authorized live/physical TUI qualification across desktop/window-manager combinations. No physical focus/flicker pass is claimed.
- **#378: blocked, NOT implemented.** `CONNECTORS.md` now lists concrete native transport, trusted exact-target picker/identity/revision, bounded operation adapter, pre-navigation/auth/screenshot policy and lifecycle/qualification prerequisites. Native family refusal remains unchanged and regression-tested. The existing external selection path does not inherit isolated-context route interception; native support must prove preventive navigation/download enforcement rather than rely on post-navigation checks. No Firefox/Safari attachment or normal-profile inspection occurred.
- Stable artifacts: `extensions/octet-browse/QUALIFICATION.md`, `CONNECTORS.md` native prerequisite section, `tests/test_background_tabs.py`, `tests/test_adapters.py`, and this execution record. Base is still `df5a7e80` plus the uncommitted scoped working diff; no release/commit/publication claim.

### Reproducible offline pinned-binding command (executed, exit 0)

This invokes installed library code with an in-memory dispatcher-shaped response,
not the browser transport. It is **not** live persistent-launch qualification.

```sh
"$HOME/.octet/browse/runtime/playwright-1.57.0/venv/bin/python" - <<'PY'
import asyncio
from importlib.metadata import version
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock
from playwright._impl._browser import Browser
from playwright._impl._browser_context import BrowserContext
from playwright._impl._browser_type import BrowserType

async def check():
    assert version('playwright') == '1.57.0'
    connection = SimpleNamespace(_loop=asyncio.get_running_loop(), _dispatcher_fiber=None, _objects={})
    browser_type = BrowserType(connection, 'BrowserType', 'type', {'name':'chromium', 'executablePath':'not-executed'})
    browser_type._playwright = SimpleNamespace(selectors=SimpleNamespace(_selector_engines=[], _contexts_for_selectors=set(), _test_id_attribute_name='data-testid'))
    browser = Browser(browser_type, 'Browser', 'browser', {'name':'chromium', 'version':'143.0.7499.4'})
    context = BrowserContext(browser, 'BrowserContext', 'context', {
        'options':{}, 'tracing':SimpleNamespace(_object=SimpleNamespace()),
        'requestContext':SimpleNamespace(_object=SimpleNamespace())})
    channel = browser_type._channel
    channel.send_return_as_dict = AsyncMock(return_value={'browser':browser._channel, 'context':context._channel})
    profile = Path('extensions/octet-browse/tests/offline-no-profile')
    assert not profile.exists()
    returned = await browser_type.launch_persistent_context(profile, headless=False)
    assert returned is context and returned.browser is browser
    assert callable(returned.browser.new_browser_cdp_session)
    assert channel.send_return_as_dict.call_args.args[0] == 'launchPersistentContext'
    assert not profile.exists()
    print('PASS: installed Playwright 1.57.0 persistent-context Python binding retains Browser parent and CDP method with dispatcher-shaped offline transport; no browser/profile created')
asyncio.run(check())
PY
```

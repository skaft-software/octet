# Final extension / SDK / application review

## Summary

Resumed review against `df5a7e809715961b9344af6b52e43a6ca48f56b3`, with changes confined to `extensions/**` and this report. The permitted review surface also included `sdk/**`, `examples/**`, and `apps/**`. Existing unrelated changes, including staged Apple build-output deletions, were left untouched. This is targeted review evidence, **not blanket security or release qualification**.

Fixed launcher host-authority handling, parent duplicate-writer exposure, herdr preflight/partial-failure reporting, MCP cross-server static-credential resolution, Pi API 0.3 offer drift, and documented vendored-SDK synchronization. Authorization, explicit enablement, native automation and OAuth boundaries remain in place. **Open-all is Partial: all product worker and parent pane execution is blocked until atomic host writer claim/settlement exists.** The integration owner corrected importer executable modes; package verification remains pending.

## Evidence

### Findings and changes

1. **Launcher: opaque handles supported, product execution blocked (Partial).** The host can resolve opaque delegated references and refuse durable live/approval states (`crates/octet-coding-agent/src/session_store.rs`, `path_for_delegated_handle`). The extension retains `launchable`, `launch_blocked`, and `live_task`, clears authority when a record disappears, refreshes `agent/list` through an owner-bound command, and rejects cached-only launch authority. **No worker pane is executable**, including fresh host-present, non-live, launchable records with no refusal: these report **atomic host writer claim/settlement unavailable**. Parent reopening stays blocked. Duplicate worker handles are refused and opaque handles remain in display-only plans. Repeated product open-all calls have zero pane effects. Low-level adapters remain directly tested without simulating a host claim. See `extensions/octet-subagents/octet_subagents/launcher.py:210`, `:295`; `orchestrator.py:830`; regressions in `tests/test_launcher.py:397` and `tests/test_orchestrator.py:647`.

2. **Herdr adapter: validate before effects; retain uncertain outcomes.** Direct adapter tests retain command-token and size checks before any split, and the validated workspace remains in the plan. Malformed nested split JSON fails without an uncaught shape error. Known pane IDs survive submission failure/timeouts; split failures with unknown effects never claim that nothing was created. Nonzero submission acknowledgement remains `command_submitted: null`, not proof that Enter was never sent. Adapter execution stops at the first failure without destroying existing panes. See `extensions/octet-subagents/octet_subagents/launcher.py:512`, `:560`; direct tests in `tests/test_launcher.py:439`. **These adapters are not executable through product open-all while atomic host ownership is unavailable.** README/reference/changelog and detached-worker presentation explicitly describe Partial status, not operator-coordinated live handover.

3. **MCP: cross-server static credential boundary fixed.** Previously, any enabled static-bearer descriptor installed a general environment provider; another server's broker-style `bearer` reference matching `OCTET_MCP_*` could then resolve that variable. The runtime now binds the provider to exact enabled streamable-http static `(server_id, variable)` pairs. The mixed-server regression confirms the broker descriptor receives no token and its HTTP fixture receives zero requests. See `extensions/octet-mcp/octet_mcp/runtime.py:54`, `streamable_http.py:76`, `tests/test_streamable_http.py:875`. Existing endpoint/network/redirect restrictions, explicit static opt-in, and unavailable stock OAuth/broker behavior were not relaxed.

4. **Pi bridge: current optional host offer accepted without new authority.** The generated API 0.3 host offer includes `theme_selection` / `theme/select`; the handwritten bridge allowlists rejected it, preventing initialization. Added those known schema names and their dependency mapping, but did not select them. Tests verify offers with/without themes, absence of selected theme authority, and rejection of an orphan method or unknown capability. See `extensions/octet-pi-compat/bridge.mjs:68`, `tests/test_bridge_protocol.py:963`. Provider/OAuth and legacy UI admission remain unchanged.

5. **SDK vendor drift fixed.** The shared Python SDK exported its inert event-bus reference implementation, while Browse/Subagents vendor copies lacked it. Synchronized `__init__.py` and `event_bus.py` under both packages. All five top-level Python SDK files are byte-identical in both vendors. This adds no host `bus/*` capability or cross-extension transport.

### Reviewed boundaries, not additional fixes

- **Computer use:** inspected runtime/policy/lifecycle and mocked-native backend coverage, including exact owner/target/action authorization, stale native identity, sensitive fields, release/settlement and fail-closed code execution. `extensions/octet-computer-use/octet_computer_use/runtime.py:111`, `:220` keeps host policy distinct from native confirmation. No new concrete defect was established in this targeted pass; trusted-local/mock evidence is not product authorization or native qualification.
- **Serve/apps:** inspected public error sanitization (`extensions/octet-serve/src/error.rs:68`, `bounds.rs:83`), loopback Host/Origin/cookie admission (`transport.rs:1971`), related security/lifecycle test sources, and web workspace/activity behavior. No new confirmed bypass was established by those reads. **The full Serve authorization/path/network surface was not audited**, and no Serve Rust tests or Apple app builds/tests ran. Web component tests do not qualify the server.
- **Importers:** inspected read-only protocol/source boundaries and exercised Cline/Aider/Pi fixtures for negotiation, bounded parsing, secret omission, symlink/path rejection and source immutability. These do not qualify installation, host ingestion/backups/restore, or real upstream runtime parity.

### Final local checks

All final checks below succeeded. Python runs used `PYTHONDONTWRITEBYTECODE=1`; no dependency installation or remote service was used.

| Surface | Command/selection | Observed result |
| --- | --- | --- |
| Subagents | Package-root unittest discovery, recursively excluding `RealTmuxTests` | 88 passed after fail-closed delivery gate; direct stub adapters only |
| MCP | Package root: `PYTHONPATH=../../sdk/python:. python3 -m unittest discover -s tests -t . -q` | 76 passed |
| Computer use | Package root: `PYTHONPATH=../../sdk/python:. python3 -m unittest discover -s tests -q` | 93 passed; mocked native |
| Shared Python SDK | `PYTHONPATH=sdk/python python3 -m unittest discover -s sdk/python/tests -q` | 101 passed |
| Browse | Package root: `PYTHONPATH=vendor:. python3 -m unittest discover -s tests -t . -q`, real-browser opt-ins unset | 82 discovered: 80 passed, 2 skipped |
| Cline / Aider / Pi importers | unittest discovery in their documented test directories | 11 / 8 / 4 passed; Pi used the existing local host binary, no build |
| Pi compatibility | Package-root unittest discovery, `OCTET_PI_REAL_PACKAGE` and `OCTET_PI_REAL_EXTENSION` unset | 74 discovered: 71 passed, 3 skipped |
| Pi UI helpers | `node --test extensions/octet-pi-compat/tests/test_semantic_ui.mjs extensions/octet-pi-compat/tests/test_editor_handoff.mjs` | 17 passed |
| TypeScript SDK | `node sdk/typescript/tests/api_v03_conformance.mjs` | 39 fixtures passed |
| Web | Existing Vitest: `src/workspace-layout.test.ts src/App.test.tsx src/components/ActivityRail.test.tsx --maxWorkers=1` | 18 passed across 3 files |
| Diff/vendor consistency | Scoped `git diff --check`; byte comparison of SDK Python files | Clean / identical |

The delivery-gate follow-up reran Subagents (88 passed) and the scoped diff check. Other results in the table are retained evidence from the preceding review, not additional reruns. Regressions cover repeated tmux/herdr requests with fresh launchable snapshots producing zero pane effects; direct adapter tests retain success, preflight and partial-failure coverage.

The Subagents selection used `unittest.defaultTestLoader.discover('tests', top_level_dir='.')`, recursively retaining test IDs without `.RealTmuxTests.` and running that resulting suite. No real/shared tmux or herdr session was touched.

Intermediate failures were investigated rather than hidden: Subagents initially exposed the vendor drift; the first MCP fix accidentally included disabled descriptors and was corrected; Pi initially reported 25 failing assertions/subtests from offer drift before the fix. One MCP rerun omitted unittest's `-t .` and failed relative imports; the documented package-context invocation above passed. Web emitted a Node experimental-localStorage warning, with no test failure.

## Uncertainty / remaining blockers

- **Unsupported atomic writer handover; fail-closed product.** `Session::persist` (`crates/octet-agent/src/session.rs`, `persist`) takes an advisory lock per append and checks stale length; it is not a lifetime exclusive writer lease. Fresh launchability plus opaque-handle resolution cannot atomically settle/claim a child across processes. Product open-all therefore blocks **all workers and the parent**, rather than allowing snapshot-based launches with a documented race. The host-owned exclusive claim/settlement primitive remains unavailable; there are no fabricated leases, invented APIs, or host refactors in this change. Open-all remains honestly **Partial**, with zero product pane effects and directly tested low-level adapters.
- **Importer release portability (mode correction integrated; verification pending):** the integration owner corrected `extensions/octet-import-pi/extension.sh`, `extensions/octet-import-aider/extension.py`, and `extensions/octet-import-cline/extension.py` from tracked 100644 to **100755**. A read-only `git ls-files -s` check confirmed all three modes after the follow-up. Fresh-checkout/package verification remains pending; local fixture success is not installed-package evidence. No git/index mutation was performed by this reviewer.
- **Unsupported primitives:** host-mediated `bus/*` remains absent; the SDK kernel is not cross-extension end-to-end support. Computer use still needs host-brokered automation authorization, target selection and settlement. Native Firefox/Safari Browse connectors remain unsupported descriptors, not Chromium substitutes. Native hardware/focus, real multiplexer, live remote MCP/OAuth, and real-Pi/full unchanged-source campaigns were not run.
- **Incomplete coverage:** Serve remains a targeted source review plus web fixtures, not a full security audit. SDK/examples and Apple apps were not exhaustively requalified. Cargo/Rust/Swift/npm install/build commands, remote actions, subagents, git mutations and cache cleanup were not used. Persisted trust policy, OAuth broker policy, clipboard image capture, tool auto-download, and chord/CBOR/unix-socket work remain excluded.

## Artifact/session references exposed by octet

- Review artifact: `docs/parity/REVIEW-extensions.md`.
- No session ID, hosted test artifact, or external evidence reference was exposed for this review. Test session handles are synthetic fixtures, not resumable review-session artifacts.

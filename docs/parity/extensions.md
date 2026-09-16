# Pi parity — extension surface

Detail owner for the extension-surface rows of the additive Pi parity delivery
ledger ([docs/parity/README.md](README.md)). The provider, codec, tool, TUI and CLI
rows stay with their own detail pages; this page records only what octet's
executable-extension surface can do, with the exact anchors and the gates that
remain open.

Reference (read-only): `earendil-works/pi` at
`8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`.

## Invariants that apply to every row

- **No persisted project-trust widening.** A capability request can never change a
  persisted trust decision. The API `0.3` capability that reaches project state is
  namespaced (`theme/select`) and fails closed for foreign namespaces
  (`protocol/extension-api-v0.3.schema.json:671` maps `namespace_mismatch` and
  `trust_widening` to `capability_mismatch`).
- **Fail-closed on unknown input.** Unknown capability, method or parameter shape is
  typed and non-retryable: `invalid_params` (`-32602`),
  `capability_mismatch` (`-32011`), `resource_exhausted` (`-32012`)
  (`protocol/extension-api-v0.3.schema.json:590`, `:596`).
- **Bounded by default.** Every surface added here is bounded in bytes, entries,
  connections and time before it can reach a peer or the filesystem.

## Extension API 0.3 host-mediated capabilities

| Capability | State | Authoritative anchors |
| --- | --- | --- |
| `event_bus` / `bus/*` | Implemented in Rust/process/product; binding/lifecycle fences and Rust/product fixtures now execute; surviving-peer session-switch evidence still missing | `crates/octet-agent/src/extension_process/event_bus.rs`; `crates/octet-agent/tests/extension_event_bus.rs`; `crates/octet-coding-agent/src/extensions/bus_tests.rs`; [bus contract](../extensions/event-bus.md) |
| `theme_selection` / `theme/select` | **Unavailable in product**; generated policy helper only | Product offer deliberately removes the capability and method in `extension_process.rs::api_v03_host_offer_for_services`; `tests/api_v03_runnable.rs::unimplemented_theme_selection_is_not_offered_and_returns_a_canonical_refusal` is refusal evidence, not implementation |
| Configuration / migration PostMutation | Typed notifications + real read-only family consumers; emitter coverage remains owner-dependent | `ExecutableExtensions::notify_configuration_changed`, `notify_migration_ingested`, `rescan_post_mutation_resources`; `extensions/mutation_resources.rs`; `extensions/hook_tests.rs`; [hook contract](../extensions/HOOK-ENRICHMENT.md) |

The bus derives publisher identity and generation from the registered process,
uses only typed scalar payloads, reserves every subscriber's bounded writer
before committing a publication, and invalidates pending frames on generation,
subscription, binding or session changes. Every request and event is scoped to
the host-issued `binding_id`, and peers that select `event_bus` must also select
the host-to-extension `bus/lifecycle` notification; a peer that cannot record a
lifecycle change is retired rather than left on an apparently current stale
binding. Subscribes distinguish an `active` subscription from a bounded `pending`
interest. Product discovery binds one bus to isolated API `0.3` processes;
legacy/shared runtimes do not acquire it. Host bus unit tests, two-process
fixtures and product-discovery fixtures execute in the current receipts
(`parity-next-agent-int-05`, `parity-next-coding-lib-04`), but no real
active-session A→B→A switch with both processes surviving has been captured, so
surviving-client recovery is not qualified.

Theme selection still needs a real extension-scoped presentation-role consumer
and host-resolved non-widening catalog. Do not turn on the offer merely because
the generated resolver passes tests, or reinterpret it as a global theme change.
Neither a bus declaration nor a theme request changes persisted project trust.

Configuration and migration rescans now understand opaque settings/MCP/skills
resource families, with distinct process and resource-revision fences. General
notification helpers are not proof of every product emitter. Configuration
commit call sites are owned by the product integration worker; the standalone
migration CLI still needs an already-authorized executable-extension observation
owner and completed-transaction result plumbing. Dry-run/no-op/partial rollback
must never emit, and no process may be started merely to manufacture notification
coverage. See the current final verification record before claiming completion.

## MCP

| Transport | State | Authoritative anchors |
| --- | --- | --- |
| stdio | Landed | [extensions/octet-mcp/README.md](../../extensions/octet-mcp/README.md), `octet_mcp/protocol.py` |
| Streamable HTTP | **Experimental and blocked by default** | `octet_mcp/config.py:251` (an enabled streamable server requires the process-owner flag), `octet_mcp/manager.py:113`, `octet_mcp/runtime.py:34` (`--experimental-streamable-http-mcp`); [nine-defect record](../../extensions/octet-mcp/REFERENCE.md#known-streamable-http-defects) |

Streamable HTTP is deliberately *not* flipped to default: the nine recorded defects
have local remediations and deterministic regressions
(`extensions/octet-mcp/tests/test_http_hardening.py`, `tests/test_streamable_http.py`),
but there is no live remote qualification.

Two previously open defects are closed locally:

- **Static, extension-scoped credentials.** `{"type": "static-bearer",
  "environment": "OCTET_MCP_<NAME>"}` is read per request from the process
  environment by `StaticEnvironmentCredentialProvider`
  (`octet_mcp/streamable_http.py:77`), is refused for any name outside the
  extension's own `OCTET_MCP_*` namespace (`octet_mcp/config.py:89`), and is never
  stored, logged, echoed in an error/diagnostic, or published through `presentation`
  or result metadata. An unset or unnamespaced name fails closed as
  `authentication_unavailable` before a socket opens.
- **Permanent GET notification stream.** Opened only when the negotiated server
  capabilities actually promise a change notification, bounded by
  `MAX_HTTP_STREAM_CONNECTIONS` (`octet_mcp/streamable_http.py:47`), reconnected
  with the committed `Last-Event-ID`, a replayed acknowledged identity failing closed
  as `sse_event_replayed`, and a `405` treated as inert (`octet_mcp/streamable_http.py:587`
  onward; regressions at `tests/test_streamable_http.py:875`, `:936`, `:987`, `:1020`, `:1064`).

**Policy-gated, not missing by accident:** OAuth discovery, dynamic client
registration, browser redirects, token acquisition/refresh, keychains, dotenv files,
persistent token stores, arbitrary static config headers and env-var fallback outside
`OCTET_MCP_*`. The exact missing primitive is a host-brokered OAuth/credential
authorization service negotiated over the extension API — a typed
`authorization/request` capability plus a host-owned token store and refresh
ownership. No self-composed browser flow substitutes for it, and a configuration file
must never be able to widen it.

## Computer use

| Row | State | Authoritative anchors |
| --- | --- | --- |
| Scoped authorization (`Scope`, `AuthorizationBinding`, `PolicyGate`) | Landed in-extension, fail-closed | `octet_computer_use/policy.py:302` (`Scope`), `:519` (`AuthorizationBinding`), `:695` (`PolicyEvaluator.evaluate_action`), `:827` (`PolicyGate`); `main.py:21`/`:121` compose the gate |
| Persistent observation/action lifecycle and trusted stop/takeover | Landed in-extension | `octet_computer_use/lifecycle.py:1430` (`LifecycleSession`), constructed with the gate at `octet_computer_use/runtime.py:82`; regressions in `tests/test_lifecycle.py`, `tests/test_policy.py`, `tests/test_runtime.py` |
| macOS native backend | Implemented, **not qualified** | `octet_computer_use/backend_macos.py`, `octet_computer_use/macos/native.py`, `tests/test_backend_macos.py` (MockNative) |
| Windows native backend | Implemented, **not qualified** | `octet_computer_use/backend_windows.py`, `tests/test_backend_windows.py` (FixtureSystem) |

The `CONTRACT.md` in that extension records the host-integration blocker: the
extension can consume an authorization decision, but nothing in API `0.3` lets a host
supply one. The exact missing primitive is a host-brokered automation authorization
service — a typed `policy/evaluate` capability with host-owned approval,
target selection and owner settlement — feeding `PolicyEvaluator.evaluate_action`,
`Scope` and the owner/target. Unknown scope denies; a stopped or replaced binding is
never reusable; stop and takeover exist only as trusted entry points, never as tool
arguments. Native macOS/Windows qualification (real accessibility trust, UIA,
human-observed takeover, process-loss release) is **hardware-gated** and no live
native run is claimed.

### Computer-use authority rows (recorded, never claimed)

| Row | State | Exact primitive still required |
| --- | --- | --- |
| [#383](https://github.com/skaft-software/octet/issues/383) host-authorized scoped actions | In-extension policy only; **policy-gated** | A host-brokered automation authorization service: typed `policy/evaluate` capability, host-owned approval, host-owned target selection, owner settlement, feeding `PolicyEvaluator.evaluate_action`. The extension already consumes such a decision; no API `0.3` host method supplies one (`crates/octet-coding-agent/src/host/*` has no computer-use/browser/desktop reference). |
| [#385](https://github.com/skaft-software/octet/issues/385) macOS native backend qualification | Implemented, **hardware-gated** | A real macOS host with Accessibility/AX trust for a selected window, plus dispatch from `main.py` (currently only the policy/protocol path is wired) and an unmocked native run; the suite injects `MockNative`. |
| [#389](https://github.com/skaft-software/octet/issues/389) Windows backend/identity/permission qualification | Implemented, **hardware-gated** | A real Windows host running the UIA/Composition adapter end to end; the suite drives a `FixtureSystem` double, not the native adapter. |
| [#390](https://github.com/skaft-software/octet/issues/390) packaged parity release | **absent / release-gated** | A release-catalog entry for `octet-computer-use` (none exists in `extensions/release-catalog.txt`), a packaged artifact, and release evidence that includes physical human takeover observation and hard process-loss release; none of it can be produced by fixtures. |

None of these rows has a live run, packaging step, or human-observed takeover in
this record. The in-extension policy, lifecycle, and both backend suites are
re-run here for regression only: `93 tests ... OK`. Authority stays fail-closed:
unknown scope denies, a stopped or replaced binding is never reusable, and stop
and takeover remain trusted entry points rather than tool arguments.

## Browse

| Row | State | Authoritative anchors |
| --- | --- | --- |
| Isolated visible Chromium operation | Landed | [extensions/octet-browse/CONNECTORS.md](../../extensions/octet-browse/CONNECTORS.md), `octet_browse/worker.py`, `tests/test_worker.py` |
| Operating an explicitly selected existing browser | Host-registered connectors only | `CONNECTORS.md:8-15` (opt-in registration; no discovery), `octet_browse/adapters.py` (`firefox`/`safari` ⇒ `unsupported_capability`) |
| Native Firefox/Safari | Not supported | `CONNECTORS.md:13`; no native operation path exists |
| Window focus stealing | Open | `octet_browse/worker.py:504` launches with `headless=False`; no activation/window-position suppression and no test asserts the TUI keeps focus |

Native Firefox/Safari operation and focus-stealing suppression are the two open
capabilities here; neither is claimed.

## Web search and subagents

| Extension | State | Authoritative anchors |
| --- | --- | --- |
| Web search | Landed (bounded public-evidence retrieval with stable source IDs) | `extensions/octet-web-search/` |
| Subagents | Landed (host-owned workers, inherited models, bounded fan-out) | `extensions/octet-subagents/`, `tests/test_launcher.py` |

## Verification status

- `python3 -m unittest discover -s tests -p 'test_*.py'` in `extensions/octet-mcp` ->
  `Ran 75 tests ... OK`.
- `python3 -m unittest discover -s tests -p 'test_*.py'` in
  `extensions/octet-computer-use` -> `Ran 93 tests ... OK` (includes the unknown/replaced
  scope, stopped-binding and trusted stop/takeover regressions).
- `python3 scripts/generate-extension-api-v03.py --check` -> exit `0`, no drift.
- `PYTHONPATH=sdk/python python3 -m unittest discover -s sdk/python/tests` -> ext-host
  reported `Ran 107 tests ... OK` with 34 bounded-bus cases; the parent later reran
  the same suite after the binding-scoped SDK repair -> **109 passed**
  (`parity-next-sdk-python`).
- Rust execution is parent-owned. The parent reran the agent and coding suites on a
  stable snapshot: every agent target **819 passed, 0 failed**
  (`parity-next-agent-int-05`) and the coding library **1521 passed, 1 ignored**
  (`parity-next-coding-lib-04`), which include the real two-process bus, product
  discovery and durable-write notification fixtures. TypeScript conformance passed
  **47 fixtures** after the stale runtime artifact was regenerated.
- **Not run here:** any live MCP remote, OAuth server or real credential; any native
  macOS/Windows automation on real hardware; any browser-window focus measurement.
  These remain the gates recorded above and are not compatibility claims.

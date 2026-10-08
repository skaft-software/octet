# Pi extension compatibility: release status and merge notes

Dated evidence for the Pi 1.0.2 compatibility work that ships in the octet 0.9.0
candidate. It records observed behaviour; it is not a frozen release or a
publication approval. Row-level detail is in the [extension API
ledger](pi-extension-api.md), and the user-facing scope is in
[compatibility](pi-compatibility.md).

## Routes

Octet runs Pi extensions in a Node subprocess through `octet-pi-compat`:

- **Path A (default).** Octet's own adapter: the `pi` API object and Pi package
  imports are Octet-written and forward to the Rust host over JSON-RPC. Tool
  authorization, sessions, persistence and the terminal stay Octet's. Anything
  unsupported is refused loudly.
- **Installed-Pi fallback (opt-in, experimental).** Same adapter and host
  boundary, but imports the shims lack or refuse resolve to the user's managed
  Pi 1.0.2 install. Selected with `--pi-runtime installed` or
  `"pi_runtime": "installed"` in `bridge.json`. See
  [pi-compatibility.md](pi-compatibility.md#installed-pi-fallback-experimental-opt-in)
  for what it gives up. Path A failures it can fix report
  `pi_compat_fallback_eligible` with the names involved.

## Verification at the 0.9.0 candidate

- Adapter suite: `npm test` in `extensions/octet-pi-compat` -> **839 tests, 812
  passed, 0 failed, 27 skipped**, exit 0 (2026-10-06, Node 25.9.0). The skips are
  the original-factory probes that require explicitly supplied reviewed
  entrypoints.
- Native module evidence (real Rust host, scripted provider): the modules below
  contain the listed number of test functions at this candidate. Run one with
  `cargo test -p octet-coding-agent --lib --locked <module>`; the scripted local
  provider replaces inference only. The filtered run behind this table reported
  `155 passed; 1 failed; 1 ignored`, and the single failure is the interactive
  acceptance recorded at the end of this section.

| Module | Tests | Covers |
| --- | --- | --- |
| `pi_baseline_contract_tests` | 5 | baseline registration and lifecycle |
| `pi_context_contract_tests` | 2 | context and prompt projections |
| `pi_exec_contract_tests` | 3 | exact argv, controlled-policy approval/refusal, timeout and cancellation |
| `pi_helper_import_tests` | 3 | public helper imports, matrix key/package helpers, absent optional properties |
| `pi_messages_tests` | 9 | text/images, public message events, persistence/resume, delivery and tool pairing |
| `pi_model_provider_tests` | 3 | model facts, scoped views, model/thinking selection |
| `pi_session_replacement_tests` | 8 | replacement, busy-turn deferral, setup/parentSession, withSession and before-hook cancellation |
| `pi_tool_hooks_tests` | 15 | tool call/result hooks, ordering, cancellation and images |
| `pi_tools_surface_tests` | 5 | authoritative catalogs/selection, late registration, startup ordering, headless exactly-once startup and failed-worker admission refusal |
| `pi_ui_contract_tests` | 12 | dialogs, overlays, editor identity, notification admission and the pre-native input lane |
| `pi_completion_tests` | 6 | composer completions through the real App, adapter and input loops |
| `pi_renderers_tests` | 1 | real App/adapter transcript renderers, persistence, invalidation, resize and Tern |
| `resource_paths::pi_app_tests` | 4 | startup/reload, recoverable callback error, failed mutation and imported default theme |
| `mcp_native_tests` | 3 | registration/replacement, execution, removal/cleanup, headless startup and missing-resident refusal |
| `pi_original_clm_tests` | 1 (ignored) | original `pi-clm`, source unavailable here |

These qualify the tested cases, not every member of a ledger row and not the
whole original extension corpus. Library-wide pass counts are not restated here:
run the suites above plus `cargo test -p octet-coding-agent` to reproduce them on
your tree.

One interactive acceptance outside these modules is red on this machine at this
tip: `typed_pi_command_is_published_and_executes_without_extension_menu` reads a
missing `trace.jsonl` (`active_run_commands_and_steering_tests.rs:876`), both in
a filtered run and in isolation. It is not module evidence for the rows above and
is not repaired by this documentation work.

## Real-package matrix (recorded 2026-10-06)

34 published Pi packages were loaded against a merged candidate build with
isolated CLI configuration and a scratch workspace (`~/.octet` untouched):
**7 pass, 12 blocked by Octet defects, 11 blocked by prerequisites the test
machine did not have, and 4 that are not extension factories.** The recorded
failures include a refused completion registration, an `extension stdout closed`
adapter exit, a PTY footer timeout in `pi-powerline-footer`, and
`session_append_process_retired` during provider-context preparation in
`pi-web-access`. Later shim and crash repairs have not been re-measured against
the matrix. See [compatibility](pi-compatibility.md#real-package-matrix-recorded-2026-10-06).

## Example extensions (recorded load and registration sample)

Pi 1.0.2 ships 79 example extensions in
`packages/coding-agent/examples/extensions`. Each was loaded with
`runner.mjs --inspect` (no host attached), first on path A, then on the
fallback if path A failed:

| Route | Count | Extensions |
|---|---|---|
| Path A | 67 | all others |
| Fallback only | 4 | `bash-spawn-hook`, `ssh`, `minimal-mode`, `built-in-tool-renderer` (Pi built-in tool factories) |
| Needs its own `npm install` (as in Pi) | 4 | `custom-provider-anthropic`, `gondolin`, `sandbox`, `with-deps`; not verified here, no installs were run |
| Deferred host feature | 4 | `custom-provider-gitlab-duo` (`registerProvider` `oauth`), `debug-provider` (`provider_stream_event`), `jev-router` (`registerVirtualModel`), `project-trust` (`project_trust`) |

Loading and registering is not behavioral qualification. The native suites above
are the behavioral evidence.

## Fixed in this work

- Non-resource factory startup: the resource phase previously returned before
  joining deferred `session_start` hooks when no resource contributor existed.
  Both frontend pumps now settle pending starts first, without loading
  resources or changing the theme. Worker/deadline/owner failures propagate;
  ordinary callback diagnostics retain their existing handling.
- Pi `onTerminalInput`: the adapter previously refused it and the host had only an
  observer-only notification. Unix input is now decoded by octet itself so every
  non-protocol event keeps the exact bytes the terminal produced, and the
  frontend hands that spelling to the negotiated `terminal_input_intercept_v1`
  lane after native reserved actions, the open slash popup and native
  search-query ownership, but before the composer slot editor. The
  adapter keeps Pi's live `Set` ordering (consume, replace, deduplicate identity,
  drop removed listeners, ignore promises) under a 50 ms whole-chain budget with
  a latched-off circuit breaker and one bounded diagnostic per owner.
- `artifact/publish` acknowledgement: the host returns `{artifact_id}` (protocol
  reference), the adapter read `id`. Every Pi image failed on the real host while
  synthetic-host tests passed because they replied `{id}`. Fixed both.
- Nested `ctx.executeTool` images: native `ImageSource::Inline` is base64; the
  adapter expected a byte array.
- `agent_settled` dispatched after `agent_end` on `turn/settled`.
- `configure.mjs` reserved every mapped hook (`d7be74f4`), including the
  provider wire hooks; while those are subscribed the host refuses every
  extension-registered provider. They are now reserved only when a factory
  registered them. Resource discovery is likewise reserved only when captured:
  ordinary factories no longer require `resource_paths_v1`, captured resource
  factories still refuse a missing consumer, and uncaptured late subscriptions
  require reconfiguration. No native host feature gate was relaxed.
- Installed-Pi fallback: Pi's built-in tool factories come from installed Pi
  and win over the path A refusal shims; `pi-ai/compat` resolves to installed
  Pi; path A reports refused built-in factories and that subpath as
  fallback-eligible.
- Stale native tests: the MCP fixture indexed a missing manifest key; the
  `pi_app` hook expectation predated subscribing every mapped hook (`d7be74f4`);
  the hook image test asserted durable media references the session format does
  not have (`ImageSource` is `Url | Inline | ProviderRef`) and now asserts the
  replacement image is persisted exactly once.
- Adapter process crashes found by the matrix: `extension stdout closed` in
  `pi-mcp-adapter`, `pi-lsp-extension` and permission-system startup refusals,
  and the completion-chain ordering race where a query that arrived immediately
  after the initialize reply was refused.
- The completed set of inherited native operation/composition failures (host
  tools reserved before catalog publication, resource hooks captured only when
  registered, the actual selected model supplied to the unchanged D11 factory)
  and the ordered-hook cancellation accounting, plus the `pi_session_replacement`
  parent-linkage and writable-setup cases.

## Unresolved compatibility work and decisions

- **Native-backed Pi editor is phase 2.** Pi's pinned editor suites are preserved
  as an upstream test corpus (193 cases), but the adapter still exports Pi's
  editor rather than a native-backed facade with synchronous native editing
  operations, and the phase-2 ownership acceptance is not green. Broader Pi
  editor parity is not claimed.
- Native-backed Pi built-in tool factories on path A: `createBashTool` and
  friends returning Pi-shaped tools that call Octet's native tools through
  `ctx.executeTool`, so they need no fallback. Pi's `read`/`edit`/`write` match
  native arguments; `bash` maps `timeout` seconds to `timeout_ms`; `grep` maps to
  `search`; `find`/`ls` have no native equivalent. Custom `operations` (as in
  `ssh`) still need the fallback.
- Host permission prompt that offers the fallback. A proposal (uncompiled) is
  summarized below; `configure.mjs` must also learn to capture with installed Pi.
- Offering native `agent_sessions` to `octet-pi-compat`
  (`crates/octet-coding-agent/src/extensions/lifecycle.rs`, currently only
  `octet-subagents`), so Pi `createAgentSession` spawns native Octet agents.
- Host features above: provider OAuth for extension providers,
  `provider_stream_event`, `project_trust`, virtual models.
- Provider inheritance and the original Anthropic-attribution callback's OAuth
  request signing; never substitute an invented credential. Reviewed
  `modelRegistry.getApiKeyAndHeaders` disclosure is the only key/header
  credential path, and it resolves one named provider/model (see the adapter
  README).
- Remaining UI/editor setters and native-to-Pi theme synchronization, Windows
  `pi.exec`, MCP HTTP/credentials. Native transcript consumers now exist; final
  original-extension qualification is separate.

### Fallback prompt proposal

Map the adapter's `-32030` initialize error to a data-free
`ExtensionRuntimeFailure::PiCompatFallbackEligible`; build the offer from
host-read `bridge.json` with recomputed entrypoint hashes; add an
`/extensions` menu item using `extension_confirmation_picker` with
`destructive: true`, default no; persist approval in user config only, keyed to
the manifest and entrypoint digests; on reload append `--pi-runtime installed`
while the approval matches.

## Using this record

This page is a dated record of the candidate's Pi compatibility work. It is not
authority to commit, rebase or overwrite another writer, and it does not
substitute for re-running the native and adapter suites after a source change.
The original rainbow and Doom/footer PTY passes qualify the observed binary, not
a frozen candidate. The earlier original `/draw` acceptance remains historical;
the currently installed `termdraw` factory is a different command, not that
fixture.

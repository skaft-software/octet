# Pi extension compatibility: release status and merge notes

Working-tree snapshot for the Pi 1.0.2 compatibility repair (draft PR #523,
stacked on #480). This records observed evidence, not a frozen release candidate
or publication approval. Row-level detail stays in the [extension API ledger](pi-extension-api.md).

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

## Current verification (2026-10-05)

- Adapter: **539 passed, 27 optional skips, zero failures** across all 60 test
  files, split into two bounded batches (566 tests) with a disposable HOME.
- Core libraries: `octet-agent` **1,066 passed / 2 ignored**, `octet-ai` **546
  passed**, `sexy-tui-rs` **268 passed**. Offline, locked, default feature profile.
- Full coding-agent library: **2,481 passed, zero failures, 3 ignored**, including
  both formerly failing startup/tool-selection cases and two new non-resource
  startup regressions. The focused tool-surface module passes **5/5**.
- Earlier focused native Pi contracts: **59 passed** across baseline, context, exec, helpers,
  messages, models/providers, session replacement, tools/hooks, UI and grouped
  notification output. A further **20-test** focused run passed renderer/cache/
  Tern, resource/theme and notification checks (some coverage overlaps).
- Original rainbow editor: paced and burst-without-grace **PTY passes** on a
  fresh native CLI. Both verify host mount/retirement identity, restored editable
  draft, resize, coordinated shutdown and terminal restoration. The permanent
  rescue hint row is gone; Ctrl+G/Ctrl+D are unchanged. Custom-editor slash-menu
  work is excluded at the user's request.
- Real App/adapter renderer acceptance passes controlled native read execution,
  canonical persistence, invalidation, resize and active Tern. Native MCP **3/3**
  passes registration/replacement, actual tool execution, removal/owner cleanup,
  headless startup and missing-resident refusal.
- Fresh original Doom/footer PTY passes gameplay, key release, resize, close and
  shutdown. A real missing close-repaint wake was fixed; no follow-up input is
  needed to restore the native composer.
- Original hashline: **8 passed** with explicit original-source paths, including
  installed-Pi text/image execution, anchored edit, artifact digest and zero
  guard violations. This is a real adapter with scripted RPC host, not native PTY.

The full coding-agent library is green; frozen-candidate release gates remain
incomplete. Non-resource factories now settle deferred startup before command
admission in both interactive and headless modes, without resource discovery or
theme reload. Startup-worker failures still block admission. Command lifecycle
ownership/reload and renderer/MCP cases pass. Clippy passes for all four libraries
with warnings denied. Repository-wide formatting still flags shared-file hunks;
those files were not rewritten over independent changes.
Original Anthropic attribution is blocked
by builtin provider inheritance and its need for a raw OAuth credential; existing
extension trust/leases do not authorize credential export. Native-theme changes
also do not yet synchronize back into Pi remote palettes. These are explicit
limitations, not compatibility successes.

Builds use one target directory with `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0` and bounded `-j 4`. Do not launch
duplicate worktree builds or delete caches while another build holds Cargo's lock.
Only inactive incremental caches were reclaimed; source and binaries were retained.

Publication remains gated on a frozen candidate, local/PR/main green, explicit
release authorization, published assets and public smoke. Nothing here declares
Pi parity or a shipped release.

## Example extensions (historical load and registration sample)

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

Loading and registering is not behavioral qualification. The native suites
below are the behavioral evidence.

## Native evidence (real Rust host, scripted provider)

The following modules were exercised through the built library test binary.
Use a disposable HOME and scrub provider credentials; only local scripted
providers/fixtures are needed. The current full-library result is listed above;
the module-level evidence below describes bounded behaviors, not full Pi parity:

```sh
CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo test -p octet-coding-agent --lib --locked --offline -j 4 --no-run
target/debug/deps/octet_sdk-<hash> <module>:: --test-threads=2
```

| Module | Result |
|---|---|
| `pi_baseline_contract_tests` | 5/5 |
| `pi_context_contract_tests` | 2/2 |
| `pi_exec_contract_tests` | 3/3 |
| `pi_helper_import_tests` | 1/1 |
| `pi_messages_tests` | 9/9 |
| `pi_model_provider_tests` | 3/3; also passes in the full-library run |
| `pi_session_replacement_tests` | 7/7 |
| `pi_tool_hooks_tests` | 15/15 |
| `pi_tools_surface_tests` | 5/5: authoritative catalogs/selection, late registration, startup ordering, headless exactly-once startup and failed-worker admission refusal |
| `resource_paths::pi_app_tests` | 4/4: startup/reload, recoverable callback error, failed mutation and imported default theme |
| `pi_ui_contract_tests` | 7/7, including overlays, dialog keys/countdowns, editor identity and terminal admission |
| `mcp_native_tests` | 3/3: valid isolated config, real registration/call/replacement/removal/cleanup, headless startup and missing-resident refusal |
| `pi_original_clm_tests` | ignored |

The older 433-test adapter / 1,037-test agent counts are superseded by the
current verification above. Native evidence qualifies the tested cases, not all
members of a ledger row or the original extension corpus.

## Fixed in this change

- Non-resource factory startup: the resource phase previously returned before
  joining deferred `session_start` hooks when no resource contributor existed.
  Both existing frontend pumps now settle pending starts first, without loading
  resources or changing the theme. Worker/deadline/owner failures propagate;
  ordinary callback diagnostics retain their existing handling.
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

The #480 repair also resolves the seven inherited native operation/composition
failures: reserve host tools before catalog publication seeds the active loadout,
capture resource hooks only when registered, and supply the actual selected model
to the unchanged D11 factory. Read-only turn observations now expose discovery
result content without replaying native registry metadata; context rewrites still
refuse that metadata. D11 and both composition tests pass in the full native
library suite. Bot-reported generated-source fixtures now parse variable values
from JSON sidecars, with hostile-string regressions.

## Unresolved compatibility work and decisions

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
- Provider inheritance and explicit credential/transport policy for the original
  Anthropic-attribution callback; never substitute an invented credential.
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

## Candidate integration notes

The working tree contains independent, uncommitted changes. Do not treat this
record as authority to commit, rebase, overwrite another writer or publish.
Run formatting checks without whole-file rewrites, inspect the final diff and
repeat relevant native/adapter tests after the last source change. The original
rainbow and Doom/footer PTY passes qualify their observed binary, not a frozen
candidate. The earlier original `/draw` acceptance remains historical; the
currently installed `termdraw` factory is a different command, not that fixture.

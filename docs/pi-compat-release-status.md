# Pi extension compatibility: release status and merge notes

Snapshot for the Pi 1.0.2 extension work stacked on PR #480
(`claude/magical-lamport-7t0feb`). It records what was observed, which route
each of Pi's example extensions takes, what is deferred, and how to merge.
Row-level detail stays in the [extension API ledger](pi-extension-api.md).

## Routes

Octet runs Pi extensions in a Node subprocess through `octet-pi-compat`:

- **Path A (default).** Octet's own adapter: the `pi` API object and Pi package
  imports are Octet-written and forward to the Rust host over JSON-RPC. Tool
  authorization, sessions, persistence and the terminal stay Octet's. Anything
  unsupported is refused loudly.
- **Installed-Pi fallback (opt-in, experimental).** Same adapter and host
  boundary, but imports the shims lack or refuse resolve to the user's managed
  Pi 1.0.x install. Selected with `--pi-runtime installed` or
  `"pi_runtime": "installed"` in `bridge.json`. See
  [pi-compatibility.md](pi-compatibility.md#installed-pi-fallback-experimental-opt-in)
  for what it gives up. Path A failures it can fix report
  `pi_compat_fallback_eligible` with the names involved.

## Example extensions (load and register)

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

Each module was run on this branch's final source with the built library test
binary, module by module:

```sh
cargo test -p octet-coding-agent --lib --locked --offline --no-run
target/debug/deps/octet_sdk-<hash> <module>:: --test-threads=2
```

| Module | Result |
|---|---|
| `pi_baseline_contract_tests` | 5/5 |
| `pi_context_contract_tests` | 2/2 |
| `pi_exec_contract_tests` | 3/3 |
| `pi_helper_import_tests` | 1/1 |
| `pi_messages_tests` | 9/9 |
| `pi_model_provider_tests` | 3/3 (one earlier in-suite run failed `pi_idle_model_setters_preserve_the_live_command_and_binding`; it passed 3/3 alone: order-dependent flake, deferred) |
| `pi_session_replacement_tests` | 7/7 |
| `pi_tool_hooks_tests` | 15/15 |
| `pi_tools_surface_tests` | 3/3 |
| `resource_paths::pi_app_tests` | 2/2 |
| `pi_ui_contract_tests` | 2/5: `custom` overlay `onHandle`, dialog keys and dialog option countdown fail their acceptance barriers (deferred) |
| `mcp_native_tests` | 1/2: the resident `octet-mcp` catalog never settles in `pi_mcp_native_app_register_replace_call_remove_and_owner_cleanup` (deferred; no example uses MCP registration) |
| `pi_original_clm_tests` | ignored |

Adapter suite (`extensions/octet-pi-compat`, synthetic host): 428 tests, 408
passed, 0 failed, 20 skipped. `octet-mcp` `tests/test_pi_registration.py`: 3
passed.

## Fixed in this change

- `artifact/publish` acknowledgement: the host returns `{artifact_id}` (protocol
  reference), the adapter read `id`. Every Pi image failed on the real host while
  synthetic-host tests passed because they replied `{id}`. Fixed both.
- Nested `ctx.executeTool` images: native `ImageSource::Inline` is base64; the
  adapter expected a byte array.
- `agent_settled` dispatched after `agent_end` on `turn/settled`.
- `configure.mjs` reserved every mapped hook (`d7be74f4`), including the
  provider wire hooks; while those are subscribed the host refuses every
  extension-registered provider. They are now reserved only when a factory
  registered them.
- Installed-Pi fallback: Pi's built-in tool factories come from installed Pi
  and win over the path A refusal shims; `pi-ai/compat` resolves to installed
  Pi; path A reports refused built-in factories and that subpath as
  fallback-eligible.
- Stale native tests: the MCP fixture indexed a missing manifest key; the
  `pi_app` hook expectation predated subscribing every mapped hook (`d7be74f4`);
  the hook image test asserted durable media references the session format does
  not have (`ImageSource` is `Url | Inline | ProviderRef`) and now asserts the
  replacement image is persisted exactly once.

## Deferred to later releases

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
- Seven pre-existing `octet-agent` failures in `extension_operations` and
  `resources_tests::composition` (operation-catalog tool counts, nested
  revocation counts, D11 refused `resource_paths_v1`); they fail identically at
  `d7be74f4`.
- The three `pi_ui_contract_tests` failures, the resident MCP catalog test and
  the order-dependent model-setter flake above.
- Native transcript renderer consumer (ledger row 25), remaining UI/editor
  setters, Windows `pi.exec`, MCP HTTP/credentials.

### Fallback prompt proposal

Map the adapter's `-32030` initialize error to a data-free
`ExtensionRuntimeFailure::PiCompatFallbackEligible`; build the offer from
host-read `bridge.json` with recomputed entrypoint hashes; add an
`/extensions` menu item using `extension_confirmation_picker` with
`destructive: true`, default no; persist approval in user config only, keyed to
the manifest and entrypoint digests; on reload append `--pi-runtime installed`
while the approval matches.

## Merge notes for agents

1. This branch is stacked on #480's head `ae7800e2`. Merge #480 first, then this.
2. Formatting: the candidate's earlier uncommitted work left `cargo fmt` drift
   in files this branch changes. The final commit formats exactly those files.
   Re-run `cargo fmt --all -- --check` after rebasing.
3. Not compiled into any target: an untracked
   `crates/octet-coding-agent/src/modes/interactive/tests/pi_mcp_registration_tests.rs`
   draft was left out of this branch; the registered MCP native tests are
   `extensions/mcp_native_tests.rs`.
4. Re-run the adapter suite (`node --test --test-concurrency=4 test/*.test.mjs`
   from `extensions/octet-pi-compat`) and the native Pi modules listed above,
   module by module with `--test-threads=2`; broad runs exceed tool deadlines.
5. No fresh release binary or PTY smoke has been run on this head.

# Verification — local octet 0.8.2 RC

Source tree: `a78080e39d6e17a68afd879cff43850f33ed15b5`.
Platform: macOS ARM64. **Test RC only; not LOCAL_GREEN or a published release.**

This inventory separates observed failures from inherited reports and unrun
acceptance. Passing unrelated tests does not close a reported defect. The list
covers the inspected PR discussion, follow-ups and surviving local artifacts;
it is not a claim of exhaustive discovery of every possible defect.

## Source and recovered work

| Component | Immutable source |
|---|---|
| Base #501, carrying #480/#495 and combined #491/#498 UI/history | `894d78ec21e74d3c058982de71a635dcb7294a2b` |
| Original #480 | `a23543de6a1e99681623254072cbb1a3e975a498` |
| #493 Responses prewarming | `d7494d8974c34f67f642ede4e42a60ce1c3f9764` |
| #494 inference metrics | `77cf12d9717fc292ad122b7fb68683c0097419ad` |
| #496 cache warming | `d18482435e29c5198159b1a4cf2d326981458214` |
| #497 codemode | `c1fea177fb71bd3f890a6b1893d0ff02bc7d61ae` |
| #499 startup | `83f175ebb2450b266ee07c3214f326dcf50e2403` |
| #500 Pi compatibility/remote UI | `c1bc241c484a3006df33ab48ae92fb8271963db3` |

Recovered the missing uncommitted Working-dot patch from
`~/github/skaft-software/.worktrees/octet-shimmer-dot-pr480-20261001`
(HEAD `6b071754`). Imported its implementation/regression and synchronized docs,
not that worktree wholesale. The candidate's
`working_activity_shimmer_crosses_its_margin_dot` regression **passed**.

#492's product removal was deliberately excluded. Startup POC implementation and
`on_screen_only` support were already carried by #501; no duplicate reapplication.
The earlier polished Tern publication/recovery artifacts likewise did not
establish another missing generation. Ancestry gaps alone are not missing-code
proof: some follow-up implementations were carried by patches/cherry-picks.

Integration repaired split-module ports, initializers, metrics labels in native
completion UI, executable launcher mode, public/embedded notes and documentation
inventory/installer whitelist. The missing historical wire-plan link was removed
rather than inventing or importing an unrelated plan. The 0.8.2 candidate's
current models.dev receipt is
`25c0f9abe330fff43d8829e8dc9dd1fc352692908680b7b0c335996059c99b00`; its
verification manifest reports the live freshness gate passing on source tree
`a78080e39d6e17a68afd879cff43850f33ed15b5`. The named log is not bundled in this
checkout, so that pass is inherited manifest evidence rather than independently
inspectable output here. The older detailed pricing/capability review in
`crates/octet-ai/models/SOURCES.md` refers to the prior `404d33...` snapshot,
not these newer projections. Sixteen test-module CRLF files were normalized to pass the whitespace gate.
Two moved raw-string Python fixtures were restored exactly from #500 after the
first test run revealed lost indentation; API lifecycle assertion wording now
matches the combined 0.3/0.4 guard. These are test repairs, not suppressed errors.

## Observed gates

| Gate | Result / evidence under `evidence/` |
|---|---|
| Optimized native `octet` and `octet-host`, default features | PASS — `release-build-final.log`, exit 0 |
| Isolated-HOME binary version/help and protocol-1 host hello | PASS — `native-smoke.exit`, `host-hello.log`; no provider calls |
| Workspace/all-target/all-feature compile | PASS — `workspace-check.log` |
| Format and release whitespace gate | PASS — `fmt-final.log`, `diff-check-final.log` |
| Live models.dev freshness | Reported PASS — `verification-results.json` records source tree `a78080e...` and log SHA; named log is not bundled in this checkout. Verify again on the final integrated source. |
| Packaged docs/inventory/link/negative-boundary tests | PASS — 391 public files, 57 extra references; `packaged-docs-final.log` |
| Release shell syntax | PASS — `release-syntax.exit` |
| Binary installer fixtures | PASS — `binary-installer-final.log`; synthetic signed assets, not public RC installation |
| Six release-catalog extension bundles | PASS — `bundle-octet-*.log`; local unsigned artifacts |
| Python SDK | PASS — 137 tests, 1 skip; `python-sdk-final.log` |
| Subagents adapter | PASS — 110 tests; `octet-subagents-final.log` |
| Web-search adapter | PASS — 54 tests; `octet-web-search-final.log` |
| MCP adapter | PASS — 102 tests; `octet-mcp-final.log` |
| Computer-use adapter | PASS — 353 tests, 7 skips; `octet-computer-use-final.log` |
| Desktop-host fixture after mode-bit guard | PASS on macOS — 11 tests; `desktop-host-final.log`; Windows unrun |
| Browse adapter | PASS — 122 tests, 3 skips; `octet-browse-final.log` |
| Codemode | MIXED — earlier 5-test suite passed; final suite 4 passed/1 failed on a 300-ms timeout/output assertion; that Node subtest passed in isolation. See `octet-codemode-final.log`, `codemode-timeout-isolated.log` |
| Optional Pi adapter | PASS — 17 tests, 5 original-extension acceptance skips; `pi-final.log` |
| Repository identity | PASS — 11 tests; `identity-final.log` |
| Python release/tooling tests | PASS — 61 tests; `tooling-final.log` |
| Canonical API 0.3 generated contract | PASS — `api-v03-final.log` |
| Rust workspace doctests | PASS — 7 tests; `doc-tests-final.log` |
| Standalone Serve backend | PASS — 298 tests; `serve-tests.log` |
| Strict all-feature workspace Clippy | FAIL — 13 dead-code diagnostics in `tui/extension_components.rs`; `clippy.log`, exit 101 |
| Full all-feature/all-target workspace tests | FAIL — 4,825 passed, 7 failed, 5 ignored across 99 reported targets; `workspace-tests-final.log`, exit 101 |
| Built embedded documentation | PASS — exact text inventory/bytes; `embedded-docs-final.log` |

Rust commands use `CARGO_BUILD_JOBS=3`, `RUST_TEST_THREADS=3`,
`RUST_MIN_STACK=16777216`, and the repository's `ci-test` profile. This does not
qualify every default-stack cold path. The stock binary build has `default=[]`;
feature-gated Serve CLI and web/desktop app artifacts are not delivered in it.

Native completion metrics tests passed for server-generation precedence,
estimated decode/unavailable fallback and never labelling E2E as decode speed.
These are deterministic tests, not live provider or vLLM accuracy measurements.
All four real-process remote-UI tests and the API 0.3 lifecycle regression passed
after fixture repairs. Earlier failing attempts remain in the evidence directory.

## Observed failed contracts — not a green release

### Cache-warming run-loop cancellation/abort

`crates/octet-agent/tests/cache_warming_run.rs`: six virtual-clock tests fail:

1. `abort_cancels_pending_warm_and_tool_without_blocking` — drop counter 0, expected 1 (:573).
2. `completion_cancels_pending_warm_without_waiting_for_its_deadline` — drop counter 0, expected 1 (:546).
3. `driven_abort_transitions_idle_mode_without_waiting_for_pending_warm` — drop counter 0, expected 1 (:610).
4. `dropping_undriven_run_cancels_warm_synchronously` — drop counter 0, expected 1 (:759).
5. `mode_change_reports_cancellation_write_failure_and_retains_exposure` — drop counter 0, expected 1 (:729).
6. `provider_poll_abort_preempts_due_warm_in_the_same_select_poll` — a warm request was observed when zero was expected (:394).

The fixture creates its drop guard inside a lazy stream. Consequently the first
five counters alone do **not** prove a real provider connection leaked; unpolled
stream lifetime/instrumentation must be distinguished from actual cancellation.
The sixth fails the no-extra-warm-request assertion. These failures are unresolved
qualification blockers, not waived tests. The `run.sh` safety wrapper defaults
the non-persisting cache-warming environment override to **off**; it does not make
the gate pass or remove the included feature. No billable warming experiment ran.

### Idle Responses prewarm positive control

`modes::interactive::tests::active_run_commands_and_steering_tests::changelog_startup_and_idle_skip_responses_context_prewarm`
timed out at `active_run_commands_and_steering_tests.rs:102`, waiting for the
ordinary idle `/status` positive-control request. The preceding negative
release-note checks are not a reproduced accidental-send defect. The same timeout
was reproduced with a single isolated test and one test thread (3.30 seconds);
see `idle-prewarm-isolated.log`. The integrated ordinary idle-prewarm path is
not qualified. A separate package-only warming probe was interrupted during
feature-graph recompilation at the harness's 90-second limit, before any test
result; it is not another warming pass or failure.

### Codemode cold-VM timeout test instability

The final five-test Python suite failed its real-VM Node suite at
`extensions/octet-codemode/tests/runtime.test.mjs:127`. The 300-ms infinite-loop
case returned the correct timeout error but no `before timeout` output. The
isolated Node subtest passed after the other validation settled. This is observed
suite instability; cold VM setup consuming the short wall budget is plausible,
not a proven root cause. Earlier full-suite passes and this isolated pass do not
turn the final full-suite failure into green. No deadline or assertion was relaxed.

### Strict lint

The dormant extension-components scaffold still produces 13 `dead_code` errors
under `-D warnings` (unused widget/title limits, placements/payloads, methods,
component slot and raw key/mouse helpers). No allow/suppression or unrelated
refactor was added to hide them. Compilation and a usable binary pass; strict
release quality does not.

## Inherited reported findings — exact-RC closure not established

Sources (also saved in `evidence/pr480-comments.json`):

- **A:** [11-finding static review](https://github.com/skaft-software/octet/pull/480#issuecomment-5946627037).
- **B:** [19-finding static review](https://github.com/skaft-software/octet/pull/480#issuecomment-5954820833).
- **C:** [prior core probes](https://github.com/skaft-software/octet/pull/480#issuecomment-5924056879), on `2f402e2b`, **not this tree**.

Original pinned locations follow; integration can move line numbers. Every row
below is **reported/unretested on the exact RC**, not a fresh reproduction.

| Ref | Finding | Original code location |
|---|---|---|
| A1 | Windows final pin can overwrite competing writes | `crates/octet-agent/src/secure_fs/imp_windows.rs:99` |
| A2 | Windows replacement crash window loses intended pathname | Same file :1546 |
| A3 | Inference cancellation can strand rotated OAuth token | `crates/octet-coding-agent/src/auth/subscription/resolver.rs:132` |
| A4/B6 | Redirect stdin blocks coordinated async cancellation | `auth/subscription/login.rs:94` |
| A5/C3 | Input planning estimate treated as guaranteed exposure bound | `crates/octet-agent/src/agent/budget.rs:386` |
| A6 | Restored child roster can lag authoritative session exposure | `crates/octet-agent/src/delegation/manager_fleet.rs:322` |
| A7 | Replaced metadata catalogs retained through `Box::leak` | `crates/octet-ai/src/model_metadata.rs:66` |
| A8 | Browser-launch children lack reaping ownership | `auth/subscription/login.rs:80` |
| A9/B4 | Windows Unicode URL/path byte slicing can panic | `crates/octet-agent/src/tools/read.rs:404` |
| A10 | Provider-controlled expiry can overflow `Instant` | `auth/subscription/device.rs:53` |
| A11 | Open extension submenu retains obsolete selection paths | `modes/interactive/extension_menu.rs:392` |
| B1 | Discovery diagnostics can reflect actual credentials | `crates/octet-coding-agent/src/app/bootstrap.rs:743` |
| B2 | Revocation leaves equivalent source-bound authority grants | `crates/octet-coding-agent/src/cli.rs:976` |
| B3 | Bounded uncertainty suppresses unknown deferred exposure | `crates/octet-agent/src/agent/deferred.rs:544` |
| B5 | Windows executable inspection misses `.exe` resolution | `extension_process/spawn.rs:628` |
| B7 | Jev readiness rejects non-Darwin `unknown` permissions | `extensions/octet-computer-use/octet_computer_use/jev_use_binding.py:144` |
| B8 | Job settlement compares authorization key with observational session ID | `jev_use_jobs.py:154` |
| B9 | Foreign-owner MCP edit persists/reconciles despite failed binding | `extensions/octet-mcp/octet_mcp/runtime.py:154` |
| B10 | Retired MCP startup can publish outside shutdown ownership | `octet_mcp/manager.py:269` |
| B14 | Renderer publication omits extension slash commands | `tui/view/renderer_model.rs:456`; omission is still visible in candidate `copy_presentation` |
| B15 | Browser-launch failure provides no OpenRouter URL | `auth/openrouter.rs:80` |
| B16 | Supplied OAuth polling interval bypasses `slow_down` increment | `auth/subscription/device.rs:75` |
| B17 | Serve directory traversal bypasses search deadline | `extensions/octet-serve/src/fs.rs:478` |
| B18 | Snap-compact renderer build failure becomes successful skip | `extensions/octet-snap-compact/test_extension.py:68` |
| B19 | Read-only GNOME status probe clears cursor-color pin | `octet_computer_use/gnome_helper.py:198` |
| C1 | Ready async lookup results arrive after another provider request | `crates/octet-agent/src/agent/turn_loop.rs:2015` |
| C2 | Clipped durable tool results lose middle diagnostics/valid JSON/readback | `agent/tool_results.rs:340`, `session/context.rs:364` |

B11's freshness blocker passed after refresh. B12's unpublished identity wording
was already corrected in #495/#501 and current identity tests pass. B13's POSIX
mode assertion is now platform-guarded; native Windows qualification remains
unrun. They are not duplicated as unchanged open failures.

### Follow-up findings requiring explicit closure

[#493 review cross-reference](https://github.com/skaft-software/octet/pull/480#issuecomment-5946749544):
ordinary Serve cold-path stack overflow; native steering after warmup; unsafe
OAuth rotation replay; warmup exposure outside hard cumulative ceilings; and
introduced Linux/macOS interactive test failure. #496's isolated claims do not
close these in this combined tree. Standalone Serve test success is not live
cold-path/provider or cross-platform qualification.

[#491 review cross-reference](https://github.com/skaft-software/octet/pull/480#issuecomment-5946785997):
OS-focus recovery race; collapsed failed local-shell diagnostics; code-block
whitespace rewriting; historical answers labelled with the currently selected
model. Candidate Tern code still contains the reported mechanisms (model label
:708, triple-backtick-only fence handling :756, default local-shell collapse,
and separately read/acknowledged focus generations). No actual-frontend
reproduction/closure occurred during this run.

[Codex connection-limit user report](https://github.com/skaft-software/octet/pull/480#issuecomment-5954835573):
`websocket_connection_limit_reached`; no safe renewal/replay root-cause fix was
established. [Bare `/fast` chooser report](https://github.com/skaft-software/octet/pull/480#issuecomment-5922803278),
with `/verbose` and `/auto-compact` in prior handoffs, likewise has no closure
receipt in this run. The broad Working shimmer/timer freeze report is not fully
closed by the narrower passing margin-dot regression.

## Unrun or failed historical acceptance

- Live provider correctness, long-duration Codex connections, credential rotation,
  exact decode-speed/vLLM comparison, warming billing/cache savings and cumulative
  hard-budget enforcement. No paid provider or new benchmark was run.
- #500's historical native custom-editor **draft rescue failed**; Pi/native startup
  comparison remains unrun and Doom aspect ratio remains incorrect. Those prior
  outcomes are not fresh RC reproductions, nor overwritten by synthetic tests.
- Optional Pi original Doom/footer/drawing/rainbow acceptance: five Node tests
  skipped for missing configured original sources/assets. No exact-tree actual
  Tern/terminal frontend qualification of those originals was performed.
- Full eight-package Pi corpus, SDK/CLI/child-agent/nesting/session-file semantics,
  unsupported image/audio/provider/runtime imports: not full Pi parity.
- Required unchanged `pi-clm`: all seven #500 gates remain pending: factory/imports,
  context-edit loop, durable ownership/resume, effective-request budgeting,
  cancellable compaction, actual native UI, and optionality/isolation.
- Windows/WSL, Linux, Intel macOS, cross-compilation/package checks, required MSRV,
  native focus/resize/clipboard/SSH/tmux/screen behavior and sustained stress.
- Live Cua/desktop permissions, Jev binding/job ownership, authenticated browser
  automation, actual external MCP servers and provider integrations where skipped.
- Full web/desktop UI build/lint/test, security/dependency audit and complete
  publication CI matrix on this tree. Remote CI cannot qualify a merely local tree.
- Canonical clean committed-source packaging, signatures/provenance, merge/main CI,
  tags/workflow/publication, public installer and public artifact smoke. None
  was authorized/performed; local archive is explicitly not the canonical release.

## Other local work preserved, not swept into the RC

Read-only audit found source leads but no retained qualification receipt for:

- Initial-provider registration barrier on a one-worker runtime.
- Extension staging `fsync` removal and chunk-based stdout framing.
- Cooked-output lock separation from renderer diagnostic writes.
- mmap partial-frame session journal and SIGKILL recovery.
- Reused validated tool-schema snapshots; borrowed terminal-gate context/tool revision.
- ASCII glob allocation reduction and configuration-authority-aware extension flags.
- CI/nextest consolidation, cache action, integration harnesses and changed-check tooling.

These remain in the dirty daily-driver checkout and were not treated as completed,
qualified deliveries. During preparation its status grew from 88 to 94 entries,
including concurrent GPT-6/Responses/steering changes. The coordinator did not
write any daily-driver path, change its branch, reset/stash/clean or import those
live edits. Both status snapshots are retained. Broken old pre-rename `.git` pointers
and unrelated worktrees were left untouched. Among inspected `/private/tmp`
artifacts only the current RC pointer remained relevant; prior temporary
verification worktrees were not recovered/recreated.

## Terminal totals and artifact verification

Final results are recorded in `evidence/verification-results.json`, with explicit
exit status, counts, failed test names and source-tree provenance. Any earlier
failed/partial commands are historical evidence, not the final gate status.

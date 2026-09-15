# Independent verification of the Pi-parity pass

Adversarial verification pass, written by a verifier that did not author any of the
rows it checks. It re-runs the commands rather than trusting the evidence files.
Where this document disagrees with a detail document or an
`docs/swarm-audit/EXECUTION-*.md` receipt, prefer this document.

- Branch: `vibe/pi-parity-roadmap-df5a7e80`
- HEAD at verification time: `9c43111dad46b9c557bf14be7428c9471a84d4b1`
  ("vibe: wave 7 checkpoint (laTeX port, codex context policy, subagents launcher, eval harness)")
- Base for diffs: `df5a7e80`
- Method: every "command run" cell below is a command this verifier ran on this
  host at the stated HEAD. No result is copied from a worker receipt.
  Un-run checks are marked `UNVERIFIED` with the exact missing precondition.

## Open items the PR body must state

1. `cargo test -p octet-ai` **fails**: `tests/client_stream.rs` has 6 failures in
   `responses_websocket_*`, and `tests/agent_run.rs` has
   `websocket_connection_limit_is_retried_by_agent` failing. All seven tests are
   untouched by this diff but exercise code it rewrote (C8).
2. `docs/parity/telemetry.md:76` claims row 3.5 is "NOT landed" with "no
   behavioral boundary test exists" — false at HEAD; the spans and three
   boundary tests exist (C1).
3. `docs/parity/providers.md:40` claims Codex `service_tier` "unblocks roadmap
   #175 `/fast`" — `/fast` is still inert by design, as its own code says (C2).
4. `docs/parity/editor.md:125-141` still calls rows 2c.3/2c.4 "Blocked" although
   the LaTeX and Mermaid modules and their tests are in HEAD (C3).
5. Rows 4.7/4.8/4.10/4.11/4.12/4.13/4.14 and several `octet-coding-agent`
   primitives are tool-/module-layer only, with **no production consumer**
   (F2, F3). tools.md says so honestly; the `octet-coding-agent` ones do not.
6. `npm test` in `apps/web` is load-sensitive: 2 of 299 tests time out under
   concurrent compilation and pass in isolation (F1). Either raise their timeout
   or run CI on a quiet machine.
7. Three redundant `#[allow(dead_code)]` attributes (`telemetry/schema.rs:156`,
   `:261`, `telemetry/spans.rs:244`) were added by this work (F5).

## Overall status

**The source tree compiles and the overwhelming majority of the behavioural
surface passes. The documentation overclaims in a few places and underclaims in
others; those are listed below and are the honest PR caveats.**

- The committed source is green: `cargo check --workspace --all-targets --locked`
  finishes clean (0 errors) at HEAD.
- The previously reported subagents fail-closed failure is **FIXED** (see §4).
- Two documentation statements are **materially false against HEAD**
  (telemetry row 3.5 "not landed"; the Codex `service_tier` "unblocks `/fast`"
  headline). One large generated test oracle is committed by accident. The
  CHANGELOG is empty despite ~96 "CHANGELOG-ready" bullets.
- One Python-suite failure is **purely environmental** (a stale `__pycache__`
  directory), reproducibly excluded by a copy of the tree (see §3).

## Contradictions (each side quoted)

**C1 — `docs/parity/telemetry.md` underclaims: row 3.5 is landed.**
The doc states (line 76 heading, line 78) "## 3.5 Span boundaries — NOT landed"
and "The seven named boundaries (…) are **not** wired into the `octet-agent` run
generator. No behavioral boundary test exists." Against HEAD:

- `crates/octet-agent/src/agent.rs:6389` `telemetry.begin_typed::<RunSpan>`,
  `:6462` `TurnSpan`, `:6863` `ProviderRequestSpan`, `:7006` `ProviderStreamSpan`,
  `:4659` / `:7195` `CompletionAttributes::record`;
  `crates/octet-agent/src/delegation.rs:3187` `begin_typed::<DelegationSpan>`.
- Boundary tests that DO exist: `crates/octet-agent/tests/agent_run.rs:9411`
  `typed_spans_nest_run_turn_provider_and_tool_boundaries`,
  `:9513` `typed_spans_label_failed_runs_without_changing_accounting`,
  `:9598` `typed_spans_cover_compaction_and_summary_boundaries`;
  `crates/octet-agent/src/delegation.rs:7316`
  `delegation_span_owns_the_child_run_and_nests_child_spans`;
  `crates/octet-agent/tests/telemetry_conformance.rs:113`
  `typed_instrumentation_nests_children_under_the_typed_span`.

The `#[allow(dead_code)]` attributes at `telemetry/schema.rs:156` (`begin_typed`),
`schema.rs:261` (`record`) and `spans.rs:244` (`context`) are also **stale** — all
three are called from the live generator (locations above). Direction of the
error is *under*claiming, but it must be corrected so the PR does not carry a
false "not landed" line.

**C2 — `docs/parity/providers.md:40` overclaims `/fast`.**
Headline: "## Codex `service_tier` (row 1a.1 — landed, unblocks roadmap #175
`/fast`)". The codec field did land (`crates/octet-ai/src/responses.rs`
`ResponsesOptions::service_tier`), but the roadmap row is **not** unblocked. The
command implementation says so itself —
`crates/octet-coding-agent/src/modes/interactive.rs:1539-1560`
(`apply_fast_command`) returns
"``/fast`` is inert: the Codex `service_tier` field exists in octet-ai, but the
live request path never sets `ResponsesOptions::service_tier` (missing primitive:
the `ResponsesOptions` builders in crates/octet-agent/src/agent.rs), so nothing
changed on the wire". The provider doc's own "Gap" paragraph names only
`applyServiceTierPricing`, not this blocker. The behaviour is fail-closed and
loud (good), but the headline is false.

**C3 — `docs/parity/editor.md:125-141` is stale (rows 2c.3 / 2c.4 "Blocked").**
`crates/sexy-tui-rs/src/rich_text/latex/{mod,tables}.rs` and
`rich_text/mermaid.rs` exist in HEAD, with `tests/latex_render.rs` (11 `#[test]`).
The doc's "no Rust equivalent in the workspace" is no longer true.

**C4 — a 398 KB generated oracle is committed.**
`crates/sexy-tui-rs/tests/_latex_diff.rs` is 398,351 bytes and tracked
(`git ls-files`), alongside `_latex_debug.rs`, `_mermaid_debug.rs`,
`_latex_probe.rs`, `_latex_stress.rs`. A prior editor receipt claimed the harness
"lives in /tmp (not committed)"; that is false. Recommendation: delete or
`.gitignore` before the PR.

**C5 — CHANGELOG.md is empty of parity work.**
`git diff --stat df5a7e80 HEAD -- CHANGELOG.md` is empty and `## [Unreleased]`
has no entries, although the parity docs ship dozens of "CHANGELOG-ready"
bullets and `docs/parity/README.md` itself requires one per item.

**C6 — FIXED since the previous verifier: subagents fail-closed policy.**
The prior receipt recorded
`test_orchestrator.py::PolicyTests::test_spawn_schema_policy_allows_whitelisted_mutation_and_rejects_outliers`
failing (`{"model": "other"}` → not rejected). Re-run on this host:
`python3 -m pytest extensions/octet-subagents/tests -q` →
**76 passed, 61 subtests passed** in 0.88s. No failure. (Test count grew from
the previously reported 55, so the owner also extended the suite.) This
contradiction is resolved.

## Contradiction C7 (blocking) — `octet-ai` tests fail and hang at HEAD

This is the most serious finding. It was reported by the previous verifier and it
is **still true at HEAD 9c43111d**; I reproduced it with my own run.

Command: `cargo test --locked -p octet-ai -- --skip reconnect_attempts_and_total_wait_are_bounded`
Result: `test result: FAILED. 346 passed; 4 failed; 0 ignored; 0 measured; 1 filtered out; finished in 2.32s` (EXIT 101).

| Failing test | Panic location | Message |
| --- | --- | --- |
| `responses_ws::tests::a_drop_before_output_reconnects_and_resumes_with_each_delta_once` | `crates/octet-ai/src/responses_ws.rs:2492` | `assertion left == right failed; left: 0, right: 1` |
| `responses_ws::tests::a_mid_stream_drop_resumes_from_the_cursor_with_each_delta_once` | `crates/octet-ai/src/responses_ws.rs:2698` | `assertion failed: connection.alive.load(Ordering::Acquire)` |
| `responses_ws::tests::failed_terminals_retire_before_publication_for_text_and_binary` | `crates/octet-ai/src/responses_ws.rs:2160` | `failure escaped before the pool key was disabled` |
| `responses_ws::tests::fatal_events_retire_before_publishing_with_a_contended_pool` | `crates/octet-ai/src/responses_ws.rs:2087` | `assertion failed: matches!(error, AiError::Transport(_) \|\| AiError::Decode(_))` |

In addition, `responses_ws::tests::reconnect_attempts_and_total_wait_are_bounded`
**hangs indefinitely**: the harness prints "has been running for over 60 seconds"
and does not finish. The previous verifier's own hung process
(`/tmp/verify7/head-target/debug/deps/octet_ai-69051c8013db57f1`) was still alive
≈50 minutes after it started, which rules out ordinary slowness.

Attribution. `crates/octet-ai/src/responses_ws.rs` is one of the files this
swarm rewrote:

- `git diff --stat df5a7e80 HEAD -- crates/octet-ai/src/responses_ws.rs` →
  `1639 insertions(+), 301 deletions(-)`; the file grew 1645 → 2983 lines.
- `git diff --stat 28c09976 HEAD -- …` (wave 7 alone) → `526 insertions(+), 37 deletions(-)`.

`failed_terminals_retire_before_publication_for_text_and_binary` and
`fatal_events_retire_before_publishing_with_a_contended_pool` both **pre-existed
at base** (`git show df5a7e80:…/responses_ws.rs` contains them); the two
`a_drop…`/`a_mid_stream_drop…` tests and the hanging test are **new**. So at
least two failures are on behaviours this branch changed. *Whether the two
pre-existing tests passed at base is UNVERIFIED* — establishing that needs a
build of the base tree, which I did not run (see "What I could not verify").

This directly qualifies the "workspace is green" claim: `cargo check` is green,
but the `octet-ai` test suite is **red and partly non-terminating**.

### Note on measurement conditions

Seven workers share this checkout. Every `cargo` invocation takes the same
`target/` lock, so at the time of this run several `cargo test` processes were
serialised (one waiting printed `Blocking waiting for file lock on build
directory`). Results below are therefore taken from runs that completed; a
blocked run is not counted as a result.

## Failure modes hunted (each with evidence)

**F1 — a claimed test pass that does not reproduce.**
The predecessor recorded `apps/web` `npm test` as "299 passed". My own run
(`cd apps/web && npm test`, 13:20 local) gives
`Test Files 2 failed | 33 passed (35)`, `Tests 2 failed | 297 passed (299)`, EXIT=1.
Both failures are `Error: Test timed out in 5000ms`:
`src/App.transcript-search.test.tsx:74` and `src/components/FleetOverview.test.tsx:110`.
Run in isolation (`npx vitest run src/App.transcript-search.test.tsx src/components/FleetOverview.test.tsx`)
both pass: `Test Files 2 passed (2); Tests 5 passed (5)` in 3.14s. So the suite is
**load-sensitive, not logically broken** — the failing tests take 1.2s / 2.0s
alone and exceed the 5s default while seven workers compile concurrently.
`npx tsc -b` is clean (EXIT=0). Verdict: `npm test` is **flaky under load**; a PR
claim of "299 passed" is not reproducible without a quiet machine.

**F2 — a "landed" primitive with NO consumer, and it is *not* documented as such.**
`crates/octet-coding-agent/src/session_store.rs:1602`
`pub fn entry_index_revision(&self) -> anyhow::Result<i64>` is **new in this
diff** (`git diff df5a7e80 HEAD` adds it) and has **exactly one reference in the
whole workspace** — its own definition (`rg -c '\bentry_index_revision\b' crates`
→ 1). The compiler agrees: `warning: method \`entry_index_revision\` is never used`.
It is not mentioned in any `docs/parity/*.md`. (Note the near-name
`entry_revision`, a different, wired primitive — this one looks like a leftover.)

**F3 — the same pattern at scale, partially documented.**
`cargo check --workspace --all-targets --locked` emits **112 `is never used`
warnings** (52 functions, 13 methods, 11 associated items, 11 structs,
11 constants, 8 enums, 2 fields). 15 of those names also appear as newly-added
definitions in this diff — e.g. `classify_reload_failure` (`tui/theme.rs:2011`),
`set_active_theme` (`tui/theme_reload.rs:336`, called only from its own test at
`:747`) and the `keybindings.rs` resolution pair (`key_event_id` at `:141` plus
its only caller `matches`). tools.md is candid about the subset it owns ("Rows
4.7, 4.11, 4.12, and 4.14 are landed as **tool-layer primitives** … documents the
exact consumer that still has to be wired"), and I confirmed each has no
production consumer: `AdaptivePreviewCoalescer` (`tool.rs:1137`) — none;
`batch_requests_termination` / `ToolOutput::requesting_termination`
(`tool.rs:1302`, `:1458`) — no loop consumer;
`collect_tool_prompt_contributions` (`tool.rs:139`) — called only from
`tests/parity_tools.rs:1000-1079`. **But the theme-reload / keybinding-resolution
primitives in `octet-coding-agent` are not documented anywhere.** The PR body
must not present them as user-visible behaviour.

**F4 — `todo!()` / `unimplemented!()` / `#[expect(dead_code)]` — CLEAR.**
`git grep -nE 'todo!\(|unimplemented!\(' HEAD` matches **only prose in
`docs/swarm-audit/EXECUTION-verify7.md`**; no source file contains such a macro.
`git grep 'expect(dead_code)' HEAD` likewise matches only docs. The code-only
diff adds zero of either.

**F5 — three redundant `#[allow(dead_code)]` added by this work.**
`crates/octet-agent/src/telemetry/schema.rs:156` (`begin_typed`), `:261`
(`CompletionAttributes::record`) and `crates/octet-agent/src/telemetry/spans.rs:244`
(`SpanGuard::context`) each carry a *new* `#[allow(dead_code)]` (the code-only
diff adds exactly these 3). All three are now called from production code
(`agent.rs:6389`, `:4540`, `:4659`, `:7195`; `delegation.rs:3187-3188`), and the
compiler lists none of them among the 112 `never used` warnings, so the
suppression is unnecessary. Harmless today; remove before the PR.

**F6 — secrets / session ids in argv or display strings — CLEAR.**
`extensions/octet-subagents/octet_subagents/launcher.py` (684 lines):
`_run` (`:446`) always calls `subprocess.run(list(argv), shell=False, timeout=…)`
and refuses empty/oversized argv; `_pane_argv` (`:316`) builds tmux argv lists;
the only string join is `_herdr_command`, and every token must first match
`_SAFE_TOKEN_RE = ^[A-Za-z0-9_@%+=:,./-]{1,512}$` (`:67`). The only environment
read is `OCTET_SUBAGENTS_OCTET_BIN` (`:136`), a binary path. No credential,
token or transcript path is read, printed or passed.

**F7 — overclaimed live/external verification — CLEAR.**
`rg -i 'verified live|ran live|smoke-tested manually|verified on hardware'` over
`docs/parity/*.md` and `docs/*.md` returns nothing. The docs state the opposite
explicitly: tools.md row 4.6 "the Windows execution path is `#[cfg(windows)]` and
is **not compiled here**: real Windows CI evidence is blocked on a Windows
runner"; extensions.md marks the macOS/Windows native backends "Implemented,
**not qualified**" and lists "macOS/Windows automation on real hardware" as
absent. No doc claims a tmux/herdr/Terminal.app live run.

**F8 — doc bullets claiming capability the code lacks — partially CLEAR.**
No parity doc claims a capability the code wholesale lacks; the errors found are
an *under*claim (telemetry.md), a stale *block* (editor.md 2c.3/2c.4) and an
*over-claim of consequence* (providers.md `/fast`). One numeric slip:
`docs/parity/extensions.md:30` says `extension_theme_selection.rs` has
"(Rust 13 + 5 tests)"; the file contains exactly 13 `#[test]` functions and the
source module has none, so "13 + 5" is not reproducible.

## Rust test surface — my own runs

All commands run with `--locked`. Timestamps are local (EDT).

| Command | Observed result | Verdict |
| --- | --- | --- |
| `cargo check --workspace --all-targets --locked` (13:13) | `Finished` in 15.47s, 0 `error` lines, 154 warnings (112 `is never used`) | VERIFIED |
| `cargo test -p octet-ai` (13:35) | lib: `351 passed; 0 failed`; then `tests/client_stream.rs`: `FAILED. 31 passed; 6 failed` (cargo fails fast and stops there) | **RED** |
| `cargo test -p octet-ai --lib -- --skip reconnect_…` (13:19) | `FAILED. 346 passed; 4 failed; 1 filtered out`, all 4 in `responses_ws::tests` | RED at 13:19 |
| `cargo test -p octet-ai --lib` (13:33) | `ok. 350 passed; 0 failed; 1 filtered out` | GREEN now |
| `cargo test -p octet-ai --lib -- --exact responses_ws::tests::reconnect_attempts_and_total_wait_are_bounded` (13:34) | `ok. 1 passed … finished in 1.79s` | GREEN now (hung earlier) |
| `cargo test -p sexy-tui-rs --no-fail-fast` (13:35) | lib `190 passed; 0 failed`; every integration target `ok` (1,1,1,5,1,6,16,27,4 …); 0 `FAILED` | VERIFIED |
| `cargo test -p octet-coding-agent --test codex_context_window` | `ok. 14 passed` | VERIFIED |
| `… --test slash_command_pty` | `ok. 7 passed` (matches tui.md's "running 7 tests") | VERIFIED |
| `… --test activity_wait_pty` | `ok. 2 passed` | VERIFIED |
| `… --test setup_cli_acceptance` | `ok. 6 passed` | VERIFIED |
| `… --test setup_tui_acceptance` | `ok. 4 passed` | VERIFIED |
| `cargo test -p octet-agent --test parity_tools --test telemetry_conformance --test read_concurrency_current --test agent_run --no-fail-fast` | **not completed** — see below | UNVERIFIED |
| `cargo test -p octet-coding-agent --lib` | **not completed** | UNVERIFIED |

Two hazards made the `octet-agent` half of this table hard to obtain, and both
are relevant to the PR:

1. **The `octet-agent` suite contains very long tests.** On my first run the lib
   target was still inside `effect::tests::file_tool_payload_contract_fits_streaming_intent_boundary`
   after >12 minutes (a pre-existing, *unmodified* test that builds two 32 MiB
   strings — `crates/octet-agent/src/effect.rs:1421`), and
   `agent::sustained_network_recovery_tests::qualified_presend_outage_waits_beyond_finite_budget_and_is_cancellable`
   printed "has been running for over 60 seconds" before passing. A later run of
   `tests/agent_run.rs` hit `qualified_codex_ws_http_cumulative_twelve_attempt_envelope
   has been running for over 60 seconds`. I did not get a clean end-to-end
   `cargo test -p octet-agent` result. **This is not proof of a hang** (unlike
   the `octet-ai` case, where the same binary ran >50 minutes); it is proof that
   the suite is slow enough that a naive CI job needs a large timeout.
2. **The shared tree stopped compiling mid-pass.** At ~13:45 both `octet-agent`
   and `octet-coding-agent` test runs aborted with
   `error[E0596]: cannot borrow \`pre_output\` as mutable … crates/octet-ai/src/responses_ws.rs:1032`
   and the same for `pre_output_bytes` at `:1033` — an *uncommitted, in-flight*
   edit by another worker. It compiled again by 13:47. Any "workspace is green"
   statement is only true between worker edits.

**C8 (open) — pre-existing `client_stream.rs` tests are broken by this work.**
`cargo test -p octet-ai --test client_stream` reproduces deterministically
(twice): `FAILED. 31 passed; 6 failed`.

| Failing test | Panic |
| --- | --- |
| `responses_websocket_connection_limit_retires_socket_and_falls_back` | `tests/client_stream.rs:914` — not `Some(Ok(StreamEvent::Started { .. }))` |
| `responses_websocket_failure_after_send_is_terminal` | `:1076` — error is not `AiError::Transport` with `phase == Body && !timeout` |
| `responses_websocket_failed_output_next_explicit_request_uses_full_http_replay` | `:1131` — same shape |
| `responses_websocket_heartbeat_timeout_after_created_is_terminal` | `:1181` — not `Started` |
| `responses_websocket_heartbeat_failure_is_terminal_and_next_request_falls_back` | `:1220` — not `Started` |
| `responses_websocket_pongs_do_not_extend_response_idle_timeout` | `:1298` — not `Started` |

Attribution is clear: `crates/octet-ai/tests/client_stream.rs` is **not touched**
by this diff (`git diff --stat df5a7e80 HEAD` lists only `src/client.rs` (+115)
and `src/responses_ws.rs` (+1639/−301)), and all six test names exist at base
(`git show df5a7e80:crates/octet-ai/tests/client_stream.rs`). So the websocket
rewrite changed behaviour those tests pinned. `docs/parity/codecs.md:131` cites
only the **lib** evidence (`cargo test -p octet-ai --lib responses_ws`, 25 tests)
and is therefore accurate as written — but the PR must say the integration
target `client_stream` is red, or fix it.

## Other suites (my own runs)

| Suite | Command | Observed |
| --- | --- | --- |
| octet-subagents | `python3 -m pytest extensions/octet-subagents/tests -q` | `76 passed, 61 subtests passed in 0.88s` |
| sdk/python | `PYTHONPATH=sdk/python python3 -m pytest sdk/python/tests -q` | `1 failed, 72 passed, 22 subtests` — environmental (see below) |
| octet-computer-use | `cd extensions/octet-computer-use && python3 -m unittest discover -s tests` | `Ran 93 tests … OK` |
| scripts | `python3 -m unittest scripts.tests.test_{changelog,source_archive,bench_pi_runtime,bench_render,bench_systems}` | `Ran 42 tests … OK` |
| catalog diff | `python3 -m unittest scripts.test_diff_model_catalog` | `Ran 10 tests … OK` |
| octet-import-aider | `python3 -m pytest extensions/octet-import-aider/tests -q` | `8 passed, 13 subtests` |
| octet-import-cline | `python3 -m pytest extensions/octet-import-cline -q` | `11 passed, 6 subtests` (the tests live at the package root, **not** in a `tests/` dir — `… /tests` collects nothing) |
| octet-import-pi | `python3 -m pytest extensions/octet-import-pi/tests -q` | `4 passed, 4 subtests` |
| extension API 0.3 | `python3 scripts/generate-extension-api-v03.py --check` | EXIT=0 |
| extension API 0.3 | `python3 -m pytest sdk/python/tests/test_theme_selection_api_v03.py -q` | `14 passed` |
| apps/web | `npm test` / `npx vitest run <2 files>` / `npx tsc -b` | see F1: `2 failed, 297 passed` under load; `5 passed` in isolation; tsc EXIT=0 |

**The sdk/python failure is environmental — proven, not asserted.**
`test_extension.py::CleanIdentityTests::test_previous_import_name_is_not_a_source_alias`
runs `python -S -c "import octet_extension; import ygg_extension"` with cwd
`sdk/python` and asserts a non-zero exit. `sdk/python/ygg_extension/` exists and
contains **only** a stale `__pycache__` (`__init__.cpython-314.pyc` dated Aug 23,
`extension.cpython-314.pyc` Sep 3 — all pre-dating this swarm), so Python 3
resolves it as a **namespace package** and the import succeeds:
`python3 -S -c "import ygg_extension"` → `OK _NamespacePath([.../ygg_extension])`.
`git status --porcelain -uall --ignored sdk/python/ygg_extension` shows
`!! …/__pycache__/*.pyc` (the directory is invisible to a normal `git status`
because everything in it is ignored). I copied the tree to `/tmp/v8/sdkcopy`,
deleted only that directory there, and re-ran:
`2 passed, 9 deselected`. So the failure is an artifact of a stale local cache
directory, **not** of this branch. Deleting `sdk/python/ygg_extension/` clears
it; no source change is needed.

## What I could not verify (and what would be needed)

- **`cargo test -p octet-agent` (incl. `parity_tools`, `telemetry_conformance`,
  `read_concurrency_current`, `agent_run`) and `cargo test -p octet-coding-agent --lib`.**
  Not obtained: the suite includes >10-minute tests and the tree was mid-edit.
  Needed: a quiescent checkout and a generous per-target timeout. Note the
  telemetry-boundary tests I *did* read exist (`agent_run.rs:9411`, `:9513`,
  `:9598`; `telemetry_conformance.rs:113`; `delegation.rs:7316`) — their
  *existence* is verified, their *pass* is not.
- **`cargo test -p octet-ai` at base (`df5a7e80`).** I did not build the base
  tree (it would need a separate `CARGO_TARGET_DIR` and a full compile), so I
  cannot say from my own run whether the 6 `client_stream` failures are new or
  pre-existing-but-newly-exposed. The tests and the file are untouched by the
  diff, which puts the burden on the author; it is not positive proof.
- **`npm run build`** for `apps/web` — not run (only `npx tsc -b`, EXIT=0).
- **Live/external qualification.** Windows CI, macOS, Terminal.app, tmux/herdr
  live panes and any real network call are unverified and unavailable here; the
  docs correctly mark them unqualified.
- **Fixed mid-pass, so my earlier observations are already stale:**
  the `octet-ai` lib failures + hang (C7: red at 13:19, green at 13:33);
  `CHANGELOG.md` (C5: empty at 13:13, +143 lines at 13:32);
  the committed LaTeX oracle (C4: tracked at 13:13; by 13:32 `.gitignore` adds
  `/crates/sexy-tui-rs/tests/_*.rs` and the five `_*` files are staged deleted).

## Remaining primitives for blocked rows

These are the concrete, code-level gaps named by the docs and confirmed against
the tree; they are what a follow-up needs, not restatements of "pending".

1. **`/fast` (roadmap #175).** A caller: `durable_responses_options` /
   `native_responses_options` in `crates/octet-agent/src/agent.rs` must set
   `ResponsesOptions::service_tier` on an endpoint whose
   `ResponsesRuntimeProfile::accepts_service_tier` is true.
   `apply_fast_command` (`modes/interactive.rs:1539`) already fail-closed.
2. **Codex websocket resumption in live runs.** `body_requests_storage`
   (`store: true`) is never set by the agent builders, so
   `client.rs` never installs a `ResponseResumer` and a post-output drop cannot
   resume (`docs/parity/codecs.md:133-143`). Same two builder functions.
3. **Codex per-request transport selection** (`sse` / `websocket` /
   `websocket-cached` / `auto`), an explicit connect deadline and debug stats —
   `1c.6` remains partial; selection is endpoint-declared today.
4. **`client_stream.rs` (6 tests) must be reconciled** with the new socket
   lifecycle before the row can be called landed (C8).
5. **Proxy seam (`1b.3`).** `crates/octet-ai/src/declarations/proxy.rs` has the
   resolver + tests; `client.rs:1891` (`reqwest::Client::builder()`) does not
   call it.
6. **Sampling-param / per-model-header merge (`1b.2`)** and per-request
   transformer hooks (`1b.1`) — data landed in `declarations/mod.rs`, merge into
   `protocol/*` and `client.rs` not wired.
7. **Tool-layer primitives with no consumer** (rows 4.7, 4.8, 4.10, 4.11, 4.12,
   4.13, 4.14): each needs its consumer — the live preview panel, the run loop's
   batch-termination check, the prompt builder, and the session's keyed
   replace/scan API for durable memos. Plus the undocumented ones in F3.

## Status table

Rows are the top-level claims a PR body would make. "Command run" is this
verifier's own invocation, not a worker receipt.

| Area | Claim under test | Observation | Command run | Verdict |
| --- | --- | --- | --- | --- |
| Workspace build | Whole workspace + all targets compile | `Finished` in 15.47s, 0 `error` lines (13:13). ⚠️ broke for ~2 min at 13:45 on an unfinished worker edit | `cargo check --workspace --all-targets --locked` | VERIFIED at 13:13 |
| Subagents policy | `unsupported_model` outlier rejected | Target test now passes; suite 76 passed / 61 subtests | `python3 -m pytest extensions/octet-subagents/tests -q` | VERIFIED (was CONTRADICTED) |
| Telemetry 3.5 | "Span boundaries NOT landed; no behavioural boundary test exists" (`docs/parity/telemetry.md:76-79`) | Spans are wired in the live generator and three boundary tests exist | `rg` over `agent.rs`/`delegation.rs`; see §2 | CONTRADICTED |
| Providers `/fast` | Codex `service_tier` "landed, unblocks roadmap #175 `/fast`" (`docs/parity/providers.md:40`) | `/fast` is inert by design; code itself says the caller is missing | `sed -n '1533,1560p' crates/octet-coding-agent/src/modes/interactive.rs` | CONTRADICTED |
| Editor LaTeX/Mermaid | Rows 2c.3 / 2c.4 blocked (`docs/parity/editor.md:125-141`) | Modules + tests are in HEAD | `ls crates/sexy-tui-rs/src/rich_text/`, `tests/latex_render.rs` | CONTRADICTED (stale) |
| Repo hygiene | No generated oracle committed | At 13:13 `_latex_diff.rs` (398,351 B) was tracked; **remediated by 13:32** — `.gitignore` now has `/crates/sexy-tui-rs/tests/_*.rs` and all five `_*` files are staged deleted | `git ls-files`, `git diff -- .gitignore` | CONTRADICTED → FIXED mid-pass |
| CHANGELOG | Parity items ship CHANGELOG entries | At 13:13 `## [Unreleased]` was empty; **by 13:32** `CHANGELOG.md` is `+143` lines with parity sections | `git diff --stat -- CHANGELOG.md` | CONTRADICTED → FIXED mid-pass |
| Extensions API 0.3 | Theme-selection surface landed | Primitive + Rust integration test + generated Python API; 14 py tests pass | `python3 scripts/generate-extension-api-v03.py --check`; `pytest sdk/python/tests/test_theme_selection_api_v03.py -q` | VERIFIED |
| sdk/python | 1 failing identity test | Environmental: stale `ygg_extension/__pycache__` makes an empty-of-source dir a namespace package | `pytest sdk/python/tests -q`; isolated copy test | VERIFIED (environmental) |
| computer-use | 93 tests OK | `Ran 93 tests ... OK` | `cd extensions/octet-computer-use && python3 -m unittest discover -s tests` | VERIFIED |
| scripts tests | 42 + 10 OK | `Ran 42 ... OK`, `Ran 10 ... OK` | `python3 -m unittest scripts.tests.test_changelog ...`; `scripts.test_diff_model_catalog` | VERIFIED |
| Dead code | No `todo!()`/`unimplemented!()`/`#[expect(dead_code)]` added | 0 hits | `rg 'todo!\(|unimplemented!\('`; `rg 'expect\(dead_code\)'` | VERIFIED |
| Secret handling | No secret/session-id in argv or display strings | Launcher builds argv lists, validates shell-safe tokens | `rg` over `launcher.py` | VERIFIED (§5) |
| Rust: octet-ai | per-crate test surface | lib `351 passed; 0 failed`; **`tests/client_stream.rs` `6 failed`** | `cargo test -p octet-ai` | CONTRADICTED (see C8) |
| Rust: sexy-tui-rs | per-crate test surface | lib `190 passed; 0 failed`; all integration targets ok | `cargo test -p sexy-tui-rs --no-fail-fast` | VERIFIED |
| Rust: named pty/acceptance targets | 5 coding-agent targets | `codex_context_window` 14, `slash_command_pty` 7, `activity_wait_pty` 2, `setup_cli_acceptance` 6, `setup_tui_acceptance` 4 — all `ok` | `cargo test -p octet-coding-agent --test …` | VERIFIED |
| Rust: octet-agent targets | parity_tools / telemetry_conformance / read_concurrency_current / agent_run | `agent_run` shows `websocket_connection_limit_is_retried_by_agent … FAILED`; suite includes >10-min tests | `cargo test -p octet-agent --test … --no-fail-fast` | PARTIAL / RED |
| Rust: coding-agent lib | per-crate test surface | not completed (tree mid-edit) | `cargo test -p octet-coding-agent --lib` | UNVERIFIED |

# Security / Fail-Closed Audit (audit9)

Branch: `vibe/pi-parity-roadmap-df5a7e80` @ 2026-09-15T17:43:07Z
Status: IN PROGRESS (written section by section)

## Surface 1 — Subagent multiplexer launcher (`extensions/octet-subagents/octet_subagents/launcher.py`)

Scope: `git diff --name-only df5a7e80..HEAD` lists `extensions/octet-subagents/octet_subagents/launcher.py`,
`tests/test_launcher.py`, `tests/test_orchestrator.py`, `REFERENCE.md`.

| Property | Evidence | Verdict |
| --- | --- | --- |
| No `shell=True`, no shell string execution | `launcher.py:454-461` is the only `subprocess.run`; `shell=False`, `list(argv)`. `rg -n "shell\s*=\s*True\|os\.system\|os\.popen" extensions/octet-subagents --glob '!vendor/**'` → only two *docstring/doc* hits (`launcher.py:14`, `REFERENCE.md:275`), no code hit. | HOLDS |
| ids/paths are separate argv elements; no metacharacter can reach a command line | Probe (stub `tmux`+`octet` on PATH recording `"$@"`, workspace dir literally named `ws ; echo PWNED ;`): recorded argv = `[new-session][-d][-s][octet-fleet-sess-1][-n][parent][-c][<...>/ws ; echo PWNED ;][--][<...>/octet][--resume][sess-1]`; `PWNED file created in cwd? False`. Metacharacter-bearing value arrives as exactly one element. | HOLDS |
| Shell-metachar id refused | Probe `plan_open_all(parent_session_id="a;rm -rf /")` → `SubagentError code=unlaunchable_session` (`_SESSION_ID_RE`, launcher.py:63,77-84). | HOLDS |
| Traversal-bearing / wrong-shape worker reference refused | Probe of `agent-session:../../etc/passwd`, 63-hex, 64-upper-hex, `/tmp/x.jsonl`, `agent-session:0` → all `unlaunchable_session` (`_SESSION_REFERENCE_RE` ^`agent-session:[0-9a-f]{64}$`, launcher.py:65,87-98). | HOLDS |
| No credential/token in argv, notice, `skipped` row, error string | Probe with `ANTHROPIC_API_KEY=OPENAI_API_KEY=SECRET_TOKEN_SHOULD_NOT_LEAK` in env: rendered plan rows + skipped row + `render_outcome` → `SECRET LEAK: False`. `skipped_worker_row` (launcher.py:609-630) emits only `id/name/state/reattachable/reason`; `Worker` (model.py:448-488) has no credential field; only env read is `OCTET_SUBAGENTS_OCTET_BIN` (launcher.py:136). | HOLDS |
| Pane cap enforced before creating anything | Probe 9 active workers → `SubagentError code=pane_cap` "would open 10 panes, above the documented cap of 9"; 8 workers → `panes 9 executable 1 blocked 8`. Cap = `MAX_ACTIVE_CHILDREN(8)+1` (launcher.py:56), checked at launcher.py:389-395 *before* `_octet_binary()`/pane build. | HOLDS |
| Missing multiplexer refuses and creates nothing | Probe (monkeypatched `detect_binary→None`, `octet` absent) → `SubagentError code=octet_missing`; upstream `multiplexer_missing` at launcher.py:371-378. Real test `tests/test_launcher.py:149-185` asserts `spawns == []` ("open-all must not run anything when the multiplexer is missing"). | HOLDS |
| No auto-download/install path | `detect_binary` is `shutil.which` only (launcher.py:122-132); no `urllib`/`requests`/`curl`/`pip` anywhere in the module (`rg` clean); the helper docstring and error text both state octet never installs. | HOLDS |
| herdr string command is metacharacter-free | `_herdr_command` (launcher.py:464-480) refuses any token failing `_SAFE_TOKEN_RE ^[A-Za-z0-9_@%+=:,./-]{1,512}$` (no `; & | $ \` ' " < > ( ) * ? [ ] { } ! #` or whitespace); `_herdr_pane_id` validates herdr's own output against the same allowlist (launcher.py:483-498). herdr also requires `HERDR_ENV=1` ownership (launcher.py:379-386). | HOLDS (by reading; `herdr` is not installed here: `which herdr` → empty, so no live herdr probe was run) |
| Parked worker is not launched unattended | `resolve_worker_pane` sets `resolvable=False` + `WORKER_PANE_BLOCKED_REASON` (launcher.py:303-313), `execute_plan` skips unresolvable panes (launcher.py:543-550); parked/detached workers are never even passed to `plan_open_all` (`orchestrator.py:857-868`) and are reported instead (`PARKED_WORKER_NOT_OPENED_REASON`, launcher.py:235-240). | HOLDS |
| Extension launcher tests pass | `python3 -m pytest tests/test_launcher.py -q` → `17 passed, 14 subtests passed in 0.66s` (incl. a real-tmux test; `/opt/homebrew/bin/tmux` present). | HOLDS |

Observations (not violations, no fix required): (a) `OCTET_SUBAGENTS_OCTET_BIN` (launcher.py:136-140) lets the
process environment choose the `octet` binary without requiring an absolute path; process env is already inside
the trust boundary (the extension can read the same env), and the value is metacharacter/space-filtered, so this
is not a fail-open path. (b) tmux/herdr stderr is echoed into the report bounded to 512 bytes (launcher.py:507,
524,534) — a multiplexer that echoed its own argv could reflect the opaque handle, never a credential.

## Surface 2 — Launchable child-session handle (`crates/octet-agent/src/delegation.rs`)

| Property | Evidence | Verdict |
| --- | --- | --- |
| Traversal-bearing / malformed reference cannot escape the session directory | `validate_launch_reference` (delegation.rs:5756-5777) requires the exact prefix + 64 *lowercase* hex, and the reference is **never** used to build a path: `resolve_launchable_child_session` reads only `session_directory.join(FLEET_ROSTER_FILE)` (a const, delegation.rs:1377,5794) and then matches `delegated_session_reference(&record.session_path) == reference` (delegation.rs:5810-5816). The token itself is `sha256(team_dir_name + "/" + child_file_name)` (delegation.rs:139-166) with the team/child names themselves allowlist-checked (team must start `team-`, ≤128 `[A-Za-z0-9-]`; child ≤256 `[A-Za-z0-9._-]`, `.jsonl`), so it cannot name a path at all. | HOLDS |
| Malformed/unknown handles fail closed with bounded reasons | Unit test `delegation::tests::a_session_owned_worker_exposes_a_launchable_handle` asserts `agent-session:not-hex` → "64 lowercase hex", `agent-session:0*64` → "unknown worker handle", `/root/worker` → "must be agent-session:<sha256>"; `cargo test -p octet-agent --lib -- launchable` → `2 passed; 0 failed`. | HOLDS |
| Parked-at-approval worker is NOT launchable for unattended mutation | Both paths check first: `launchability` (delegation.rs:5707-5734, `AwaitingApproval => Err("...parked at the approval boundary...")`) and `resolve_launchable_child_session` (delegation.rs:5817-5822). Tested twice (delegation.rs:7388-7398, 7447-7461) and re-verified by the run above. | HOLDS |
| Live in-process worker refuses hand-over | `launchability` `record.live_task => Err("a live worker owns this session in the current process; stop or detach it first")` (delegation.rs:5716-5726); the extension-facing row carries `"live_task": record.live_task` + `"launchable": blocked.is_none()` + `"launch_blocked": blocked` (delegation.rs:5989-5993) and the verdict is asserted in `crates/octet-agent/tests/delegation.rs:1448-1462`. | HOLDS |
| Vanished transcript / no roster fails closed | `!record.session_path.exists()` → "the worker session file is gone" (delegation.rs:5823-5827); missing roster → "no session-owned delegation roster in this session: {error}" (delegation.rs:5794-5803). Test asserts both (delegation.rs:7463-7486). | HOLDS |
| Extension surface sees the opaque token, never the transcript path | Extension-facing `list` rewrites the row: `value["session"] = delegated_session_reference(&record.session_path)…unwrap_or(Value::Null)` (delegation.rs:1076-1080) *after* `agent_record_value` (which does hold the raw `"session": record.session_path`, delegation.rs:5971) — the rewrite is unconditional, so a resolved reference or an explicit `null` is always what an extension receives. `LaunchableChildSession.session_path` is documented host-only (delegation.rs:5736-5744) and no production caller serialises the struct. | HOLDS |
| No secret in the token or a refusal | Token = sha256 of the random private `team-*` directory name + child filename (delegation.rs:157-166). Refusal strings are static text plus `secure_fs`/`serde_json` errors (roster path only, no content, no credential). No `env`/token read exists on this path. | HOLDS |
| `SessionDelegationHandle` debug does not print records | `impl Debug` prints only `session_directory` (delegation.rs:5884-5892). | HOLDS |

Observation, not a violation: `resolve_launchable_child_session` is publicly reachable (`pub mod delegation`, lib.rs:74)
but has **no production caller** — `rg -n "resolve_launchable_child_session|launchable_child_session" crates` finds only
the two definitions, the `SessionDelegationHandle` wrapper, and tests. So `octet --resume agent-session:…` still cannot
open a delegated child, and the extension launcher keeps every worker pane `resolvable=False` (surface 1). That is
fail-closed (nothing is launched rather than the wrong session), not a trust-boundary defect; completeness/parity of
the wiring is `verify8`'s axis, not mine. Second observation: `resolve_launchable_child_session` does not check that the
roster's `session_path` lies inside `session_directory` (the in-process loader does pin `fleet.root_session`,
delegation.rs:1992); the roster is owner-private and equally trusted, so this is not a widening path.

## Surface 3 — Extension API 0.3 theme selection (`theme_selection` / `theme/select`)

| Property | Evidence | Verdict |
| --- | --- | --- |
| Unknown role rejected with a typed code | `role` is a closed enum in the wire spec (`extension_api_v03.rs:879`) and `THEME_ROLES` (`:1099`); `validate_theme_select_params` fails `-32602 invalid_params`, asserted by `extension_theme_selection.rs:83-90`. | HOLDS |
| Unknown scope rejected; project/global scope unrepresentable | `scope` values are `["extension"]` only (`extension_api_v03.rs:881`); `scope_cannot_be_widened` iterates `project/global/workspace` → `-32602` (`extension_theme_selection.rs:96-110`). | HOLDS |
| Unknown field rejected | `#[serde(deny_unknown_fields)]` on `ThemeSelectParams` (`extension_api_v03.rs:564-569`); test sends `"persist": true` and expects a rejection (`extension_theme_selection.rs:139-160`, `unknown_fields_are_rejected`). | HOLDS |
| Unknown theme id rejected with a typed code | `resolve_theme_selection` catalog lookup miss → `ThemeSelectionRejection::UnknownTheme` = `invalid_params` (`extension_api_v03.rs:1124-1126`, `:1104-1113`); test asserts `-32602` + "unknown theme id" (`extension_theme_selection.rs:74-80`). | HOLDS |
| Namespacing: one extension cannot satisfy another's request | `params.namespace != requesting_namespace` → `NamespaceMismatch` = `capability_mismatch` (`extension_api_v03.rs:1122`, error code asserted `-32011` in `extension_theme_selection.rs:46-64`). The check is *equality against the caller-supplied namespace*, i.e. the policy function is correct but depends on the host passing the authentic requesting principal. | HOLDS inside the policy function; production wiring UNVERIFIED (see below) |
| Trust-widening theme refused | Vocabulary gate `THEME_TRUST_VALUES == ["compiled","user"]` (`extension_api_v03.rs:1100`, `:1119`, `:1127`); `widening_trust_is_rejected` asserts trust `"project"` → `-32011 capability_mismatch`, and `unknown_trust_value_is_rejected` asserts `"root"` → `-32011` (`extension_theme_selection.rs:112-137`). | HOLDS |
| Cannot touch persisted project trust | The only scope value is `extension` (`:881`); the capability's result has no trust/scope field at all (`ThemeSelectResult` = `status`/`theme_id`/`reason`, `:575-581`), so a selection cannot express or write a trust level. The generator independently enforces this at build time: `scripts/generate-extension-api-v03.py:385` `"theme_selection must stay scoped to the requesting extension"` and `:395` `"theme_selection trust vocabulary must not overlap the extension namespace"`. | HOLDS |
| Overlong theme id bounded | `MAX_THEME_ID_BYTES = 128` (`:48`, `:879`); `overlong_theme_id_is_rejected` asserts `-32012 resource_exhausted` (`extension_theme_selection.rs:151-160`). | HOLDS |
| Generated SDK/Python parity | `sdk/python/octet_extension/api_v03.py:947-961` implements the same order and the same two typed codes; tests: `cargo test -p octet-agent --test extension_theme_selection` → `13 passed; 0 failed`; `cd sdk/python && python3 -m pytest tests/test_theme_selection_api_v03.py -q` → `14 passed`. | HOLDS |

Observation (fail-closed, not a defect): `theme/select` is *advertised* (`available: true`, in the host offer at
`extension_api_v03.rs:106`, `:920`, `:925`) but the host dispatcher has no arm for it, so an extension that sends it
falls into the catch-all at `crates/octet-agent/src/extension_process.rs:13751-13772` and receives JSON-RPC `-32601
"method not found: theme/select"`. `resolve_theme_selection` also has **no production caller** (`rg -n
"resolve_theme_selection" crates/*/src` → only the generated definition at `extension_api_v03.rs:1120`), so the
enforcement above is currently *vacuous in production*: nothing is applied, nothing can widen. The advertised-but-
unimplemented surface is a parity/completeness gap for `verify8`; it cannot widen trust, so it is not an audit
violation. It would become one the moment a host handler passes an extension-controlled value as
`requesting_namespace` — that is the invariant to preserve when the handler is written.

## audit11 continuation — surfaces 4–8 + re-verification sample

`START 2026-09-15T17:55:33Z audit11 alive` appended to `docs/swarm-audit/EXECUTION-audit9.md`. Surfaces 1–3 are the
predecessor's; audit11 re-verified a sample, then audited surfaces 4–8. Probes ran at HEAD `00e3ca3e` with
`target/debug/octet` built from this tree, in the shared dirty worktree; `crates/octet-agent/src/agent.rs` and
`crates/octet-coding-agent/src/session_store.rs` were being edited by other workers *while* this audit ran, so
their line numbers are worktree-time. Every claim is a command run or a file read; UNVERIFIED is stated where a
precondition was missing.

### Re-verification sample (surfaces 1–3, predecessor HOLDs)

| Property | Evidence | Verdict |
| --- | --- | --- |
| S1 metacharacter workspace stays exactly one argv element, nothing executed | Stub `tmux` on PATH recording `"$@"`; directory literally named `ws ; echo PWNED ;`; `plan_open_all(multiplexer="tmux", parent_session_id="sess-1", workers=[], workspace="<tmp>/ws ; echo PWNED ;", environment={})` then `execute_plan` → recorded argv `['new-session','-d','-s','octet-fleet-sess-1','-n','parent','-c','<tmp>/ws ; echo PWNED ;','--','<…>/octet','--resume','sess-1']`; `ls <tmp>/PWNED` → `NO`. | HOLDS |
| S1 launcher suite | `cd extensions/octet-subagents && python3 -m pytest tests/test_launcher.py -q` → `17 passed, 14 subtests passed in 0.73s`. | HOLDS |
| S2 malformed handle refused before any filesystem work; parked/live/vanished each bounded | `cargo test -p octet-coding-agent --lib -- worker_handle` → `3 passed` (`a_malformed_worker_handle_is_refused_before_any_filesystem_work`, `a_launchable_worker_handle_resolves_to_the_child_transcript`, `every_unlaunchable_worker_handle_refuses_with_a_distinct_bounded_reason`). `path_for_delegated_handle` (worktree `session_store.rs:2482-2495`) validates `delegated_handle_digest` (`:872-879`, exactly 64 lowercase hex) **before** `resolve_launchable_child_session`, refuses `pending`/`running` records (`:885-891`), then `confine_delegated_session_path` (`:930-963`) rejects `..`, symlinked team dirs and non-regular children. | HOLDS at store+resolver level; end-to-end CLI level UNVERIFIED (see residual unknowns) |
| S2 lib-level launchable-handle tests | `cargo test -p octet-agent --lib -- launchable` → `2 passed`. | HOLDS |
| S3 theme-selection policy | `cargo test -p octet-agent --test extension_theme_selection` → `13 passed`. | HOLDS |

### Surface 4 — catalog publish gates (`crates/octet-coding-agent/src/cli/catalog_publish.rs`)

Each gate refuses before `install_immutable`: checksum over the exact bytes (`:127-135`), `deny_unknown_fields`
schema (`:76-85`, `:138-146`), min-client parse + satisfy (`:148-170`), required-provider set equality +
availability (`:172-191`), entry count (`:193-200`), immutable destination (`:202-204`, `:221-284`).
`read_regular_file_bounded` (`:121-125`) refuses symlinks and caps the source at 16 MiB (`:28`).
`install_immutable` refuses any pre-existing path — including a dangling symlink or a directory — via
`symlink_metadata`, writes a fresh 0600 temp (`create_new` + `sync_all`), and publishes with `hard_link`, so a
concurrent publisher cannot win and no refusal can truncate the previous artifact (`:221-284`).

Live CLI probe (`./target/debug/octet catalog publish …`), source = 2-entry `octet-catalog-1` doc:

| Property | Command + observed output | Verdict |
| --- | --- | --- |
| Checksum mismatch refuses and creates nothing | `--expected-checksum 000…0` → `Error: catalog publish refused (checksum): expected 0000…, computed c83e43d8…`; `fresh exists: NO` | HOLDS |
| Entry-count mismatch refuses | `--expected-count 3` → `refused (entry-count): document has 2 entries, expected 3`; destination absent | HOLDS |
| Required-provider mismatch refuses | `--require-provider anthropic` vs doc `["openai"]` → `refused (required-provider): document declares {"openai"}, request requires {"anthropic"}` | HOLDS |
| Min-client gate refuses an older client | doc `99.0.0` → `refused (minimum-client-version): document requires 99.0.0, this client is 0.7.6` | HOLDS |
| Unknown schema / unknown field refuses | `octet-catalog-999` → `refused (schema): document schema "octet-catalog-999", expected "octet-catalog-1"`; `"extra":1` → `refused (schema): invalid document: unknown field \`extra\`` | HOLDS |
| Existing destination left byte-identical | previous file `previous catalog`; valid publish → `refused (immutable-path): destination already exists`; `prev-content: [previous catalog]` | HOLDS |
| Dangling-symlink destination refuses, symlink intact; missing dir refuses | `ln -s missing-target dangling.json` → `refused (immutable-path): destination already exists`; `--destination nodir/x.json` → `destination directory does not exist` | HOLDS |
| Symlink source refused | `src-link.json -> file` → `refused (source): Too many levels of symbolic links (os error 62)` (no-follow read) | HOLDS |
| Success is exact bytes, owner-only, no temp residue | `Published catalog to … (2 entries, sha256 c83e43d8…)`; `bytes-identical: True mode: 0o600`; `leftover temp files: []` | HOLDS |
| Module tests | `cargo test -p octet-coding-agent --lib -- catalog_publish export_html` → `9 passed` | HOLDS |

Observation, not a violation: `expected_checksum` is compared case-insensitively after `trim()` (`:130`);
uppercase hex is accepted, which carries no entropy and cannot satisfy a wrong digest.

### Surface 4b — HTML session export (`crates/octet-coding-agent/src/modes/export_html.rs`)

| Property | Evidence | Verdict |
| --- | --- | --- |
| Script-free document | `render` emits no `<script>` and no event attributes; metadata and every record go through `escape()` = `sanitize_text` defaults + `&<>"'` entity escaping (`:5-8`, `:53-62`). Test fixture contains `<script>alert('x')</script>`, an OSC-52 escape, `<img src=x onerror=evil>` and asserts they are absent/escaped (`export_html.rs:71-84`). | HOLDS |
| Escaping CSP | `default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'` (`:53`); the real-CLI test asserts the string (`tests/parity_cli.rs:939`). | HOLDS |
| Only bounded inline raster payloads become markup; no fetch/embed of anything else | `images()` (`:12-45`): only `Image.source.Inline`, base64 decode, PNG/JPEG/GIF/WebP magic, ≤5 MiB decoded (≤7 MiB encoded), re-encoded from decoded bytes into attributes, then source replaced by an inert placeholder; audio → placeholder; URLs, SVG, unknown payloads produce no markup. `rg 'reqwest|http|fetch|std::net' export_html.rs` → no network symbol. | HOLDS |
| Probes | `cargo test -p octet-coding-agent --lib -- catalog_publish export_html` → 2 export tests passed; `cargo test -p octet-coding-agent --test parity_cli -- export_html` → `sessions_export_html_is_a_single_script_free_self_contained_file … ok` (real binary + loopback provider; asserts exactly one `.html` file, no `<script`, no `onerror`). | HOLDS |

### Surface 5 — media admission / durable usage

| Property | Evidence | Verdict |
| --- | --- | --- |
| Explicit admission retained, bounded by policy | Owner presentation copy (`crates/octet-agent/src/tool.rs:1471-1512`) admits only `ImageSource::Inline`: ≤4 images, ≤2 MiB each, ≤4 MiB total; over-limit/remote sources set `presentation_images_omitted` and are skipped, never truncated; accepted bytes are copied out of the backing allocation so an 8 MiB transport buffer is not retained. Observer copies are stripped by `without_media_payloads[_for]` (`:1595-1620`). | HOLDS |
| Media bounds probes (octet-agent) | `cargo test -p octet-agent --lib -- owner_presentation without_media media` → `15 passed`, incl. `owner_presentation_has_independent_count_byte_and_source_bounds`, `owner_presentation_detaches_small_slices_from_large_backing_allocations` (debug output asserted free of the URL and payload bytes), `local_media_size_is_rejected_before_buffering`, `local_media_extension_must_match_magic_bytes`, `parallel_tool_results_stay_ahead_of_adjacent_media`. | HOLDS |
| No path where accounting is silently lost | Durable uncertainty is append-before-memory (`session.rs:2056-2078`: validate → `persist(&buffer)?` → push). `cargo test -p octet-agent --lib -- usage_totals partial_assistant_frames uncertainty delegated_usage` → `13 passed`, incl. `usage_uncertainty_append_failures_leave_memory_and_disk_unchanged`, `usage_totals_fold_tool_turns_and_summaries_and_preserve_uncertainty`, `delegated_usage_is_durable_and_contributes_exact_session_cost`, `repeated_delegated_uncertainty_mirroring_is_idempotent_and_keeps_known_subtotal`. | HOLDS |
| Partial-assistant journals cannot disturb accounting and are bounded | Journal is a disposable owner-only sidecar, never a session record/context/usage input (`session.rs:2893-3010`), 1 MiB + 8192-frame bounds (`:2901`, `:2942`), torn tail dropped, consumed once and removed (`take_partial_assistant`, `:3037-3060`); tests in the run above (`partial_assistant_frames_survive_reopen_and_republish_once`). | HOLDS |
| Invocation durability fails closed | `tools/durability.rs`: replay effect runs only when the capability is live *and* the memo is absent; a settled-invocation read is an error, never `None`; values/names/counts/live invocations capped (`:26-30`, `:55-63`, `:551`, `:564`, `:68`). `cargo test -p octet-agent --test parity_tools` → `17 passed` incl. `deferred_suspension_requires_a_valid_handle_and_rejects_every_mismatch`, `deferred_polls_need_one_permit_per_pass_and_fail_closed_on_stale_duplicate_or_foreign_handles`, `invocation_memos_survive_replay_until_the_outcome_is_known`. | HOLDS |

### Surface 6 — telemetry accounting integrity

| Property | Evidence | Verdict |
| --- | --- | --- |
| Spans/adapters are observation-only and cannot change accounting | `telemetry/spans.rs:1-12` states accounting is out of scope and that dropping to `NOOP_TELEMETRY_CONTEXT` loses observations, never accounting; the inert context is `backend: None, parent: None` (`:101-104`); every callback call is panic-suppressed (`passive`, `:128-130`) and after settlement is inert (`:213-236`). | HOLDS |
| In-memory adapter is bounded and cannot drop business callbacks | `MemoryBackend::start` refuses over-limit spans and counts `dropped` (`spans.rs:336-382`, `:469-475`); `cargo test -p octet-agent --test telemetry_conformance` → `9 passed`, incl. `recording_adapter_enforces_bounds_without_dropping_callbacks` (4 callbacks run, 2 spans retained, `dropped_spans()==2`) and `inert_and_in_memory_spans_never_change_accounting_outcomes` (`telemetry_conformance.rs:204-222`: identical result under NOOP and recording). | HOLDS |
| `--telemetry` JSONL path intact | `cargo test -p octet-agent --test telemetry_conformance` → `jsonl_observer_path_is_untouched_and_records_usage_uncertainty … ok` (`telemetry_conformance.rs:192-201`); all 14 `cargo test -p octet-agent --lib -- telemetry` tests passed (`writes_bounded_machine_readable_records_without_raw_arguments`, `policy_decision_records_are_secret_safe_and_machine_readable`, `run_usage_includes_compaction_when_the_following_request_never_finishes`, …). | HOLDS |
| `has_uncertain_usage` fail-closed and durable | `Session::has_uncertain_usage` is "any durable uncertainty record on any branch" (`session.rs:2078-2081`); `record_usage_uncertainty` validates then persists **before** mutating memory (`:2056-2078`); `usage_uncertainty_serializes_without_fictional_usage_or_payloads`, `usage_uncertainty_survives_repair_of_a_later_torn_append`, `durable_uncertainty_blocks_later_token_and_cost_ceilings` passed in `cargo test -p octet-agent --lib -- usage_totals partial_assistant_frames uncertainty delegated_usage` → `13 passed`. | HOLDS |
| Totals are complete, not silently truncated | `UsageTotals::from_records` folds every `UsageRecordKind` (`telemetry/schema.rs:288-330`); completion attributes carry `has_uncertain_usage` so an observer never reads a fabricated zero (`:200-266`); the session test `usage_totals_fold_tool_turns_and_summaries_and_preserve_uncertainty` asserts the totals are unchanged by recording uncertainty. | HOLDS |
| Codex above the 272K tier is recorded UNCERTAIN, not exact | `codex_context_uncertainty_operation` returns the operation only for the Codex endpoint with effective window >272K (`bootstrap.rs:4903-4907`); `record_codex_context_uncertainty` is sticky/at-most-once and called at every launch boundary including mid-session switches (`:4919-4932`, call sites `:6105`, `:6374`); `CodexContextWindow::has_uncertain_usage = context_window > 272_000` (`codex_context.rs:559`). Probes: `cargo test -p octet-coding-agent --lib -- codex_context ephemeral_accounting` → `14 passed` incl. `an_above_standard_tier_route_marks_the_session_uncertain_once`, plus `session_store::tests::ephemeral_accounting_keeps_usage_and_uncertainty_without_the_transcript`; `cargo test -p octet-coding-agent --test codex_context_window` → `14 passed` incl. `the_above_standard_tier_operation_id_is_stable` (luna 372K ⇒ UNCERTAIN, `= Some("codex-context-above-272k")`; every capped family ⇒ `None`) and `the_luna_note_states_372k_consistently_and_never_reads_as_a_cap_to_272k`. | HOLDS |
| UNCERTAIN is visible, not a silent downgrade | Frontends print the UNCERTAIN note (`commands.rs:461-477`), the surface row carries `UNCERTAIN` (`interactive.rs:8230-8237`), and the ephemeral print path warns that the totals are known subtotals (`modes/print.rs:51-53`, `:223`). | HOLDS |

### Surface 7 — Codex context-window override (`crates/octet-coding-agent/src/codex_context.rs`)

Because `octet-coding-agent` did not compile for part of this audit (another worker's in-flight
`tui/view/reasoning_render.rs`), the pure policy module was additionally compiled standalone from the worktree
source (`codex_context.rs` imports only `std::fmt`): `rustc --edition 2021 -o probe ctx.rs && ./probe`.

| Property | Evidence | Verdict |
| --- | --- | --- |
| Un-acknowledged above-cap value fails closed | Standalone probe on HEAD source: `resolve_codex_context_window("gpt-6-astra", Extended, 272000, 872000, None, raising(500000, false))` → `Err(OverrideRequiresAcknowledgement{requested:500000, working_context_window:272000})`; error text quotes the exact wording. The same file test and the CLI test agree (`codex_context.rs:663-710`; `cli::tests::codex_context_window_flag_parses_and_fails_closed_above_the_cap` passed in the coding-agent run above). | HOLDS |
| Above-entitlement value fails closed even when acknowledged | Probe: `raising(1_000_000, true)` → `OverrideAboveEntitlement` with text "octet will not request a window the model is not entitled to"; `raising(1_000, true)` → `OverrideBelowMinimum` (16384). | HOLDS |
| Non-entitled plan cannot raise above the cap | Probe: `tier=Default, raising(500000, true)` → `OverrideRequiresEntitlement`. The CLI separately refuses tokens above the family maximum before the model is known (`cli/parity.rs:81-99`). | HOLDS |
| Acknowledgement wording names BOTH consequences | `CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING` (`codex_context.rs:88-89`) contains `double-priced`, `2x input and 1.5x output`, `not only the excess`, `websocket`, `272K`; probe printed `double-priced=true websocket=true says-not-only-excess=true`. | HOLDS |
| Nothing silently raises the window | `apply_override` returns `requested` only after every gate (`codex_context.rs:563-601`); bootstrap applies the env override once at catalog registration and a refusal prints a warning then resolves again with `CodexContextOverride::NONE` (`bootstrap.rs:4850-4877`, `:5045-5067`); the TUI menu shows the wording, re-validates through `raise()` (`commands.rs:520-533`) and only prints `raise_instruction` — it never sets the env (`pickers.rs:1122-1130`, `commands.rs:539-545`). Probe of the accepted case: `context=500000 override_applied=true uncertain=true`; `CodexContextOverride::parse` rejects `"many"` and `"maybe"` (`parse-garbage-bad=true parse-ack-bad=true`). | HOLDS |
| 372K luna route is UNCERTAIN without any override | Probe: `luna context=372000 override_applied=true uncertain=true`; the no-override probe shows `clamp=true uncertain=false` for astra (cap kept, exact). | HOLDS |

Observation with a smallest fix (not scored as a violation): the override's entitlement ceiling is the checked-in
table `entitled_max_context_window(model_id)` (`codex_context.rs:479-494`, used at `:516-529`); the backend's
authenticated `discovered_max_context_window` is passed in but never used as the bound, although the module docs
say "the advertised maximum is what bounds the explicit override" (`:477-478`). If discovery advertises less than
the table (e.g. a plan-limited astra max of 400K), an acknowledged, tier-entitled request for 500K is accepted and
`context_window` (500K) exceeds `advertised_context_window` (400K). It is gated by Pro/ProLite + acknowledgement
and is recorded UNCERTAIN, so it is not an unguarded widening; the smallest fix is
`let entitled_max = entitled_max_context_window(model_id).min(discovered_max_context_window.max(1));` in
`resolve_codex_context_window`.

### Surface 8 — fail-open scan of changed code

Method: `git diff --name-only df5a7e80..HEAD` (`295` paths; ~61 remain dirty from other workers), added lines
only (`git diff -U0`), patterns `unwrap()`, `expect(`, `panic!`, `unreachable!`, `todo!`, `unimplemented!`,
`let _ =`, `.ok();`, `unwrap_or_default()`, `#[allow(dead_code)]`, `#[expect(dead_code)]`; production scope =
`crates/**/src` + `extensions/**` minus `tests/`, `*_tests.rs`, `*tests.rs`; hits after the first `#[cfg(test)]`
marker in a file were counted but treated as test code. Raw counts: 556 added-line hits in 51 "src" files, of
which 88 survive the test filter — 0 `unwrap()`/`expect(` outside the 6 reviewed invariant sites below.

| Property | Evidence | Verdict |
| --- | --- | --- |
| No `todo!()` / `unimplemented!()` added | `git diff -U0 df5a7e80..HEAD -- 'crates/**/*.rs' 'extensions/**/*.py' | grep -cE 'todo!\\(|unimplemented!\\('` → `0`. | HOLDS |
| No `#[allow(dead_code)]` / `#[expect(dead_code)]` added on anything presented as a feature | Same command with the dead-code patterns → `0`. (Branch prose in `docs/parity/*` mentions a transient `#[allow(dead_code)]` pair; the final HEAD tree has none from this work — `git grep` finds only pre-existing sites such as `app/mod.rs:222`, `tui/keymap.rs:16`.) | HOLDS |
| `unwrap()`/`expect(` in added production lines are invariants only | `tools/edit.rs:220` `chars().next().expect("nonempty match")`; `tools/find.rs:51` `stdout.take().expect("piped fd stdout")` (child spawned with `Stdio::piped`); `tools/grep.rs:26` `def.parameters["properties"].as_object_mut().expect("search schema properties")`; `crates/octet-ai/src/protocol/openai_responses.rs:516` `serde_json::to_value(tool).expect("Responses tool serializes")`; `session_store.rs:1476` `u64::try_from(read_limit).expect("session byte limit fits u64")` (bounded by the 16 MiB const); `modes/rpc.rs:114` `value.as_object_mut().expect("event object")` (guarded by the `message_update` type check directly above). All are input-independent construction invariants; none can be reached from provider/model-controlled data to bypass a check. The remaining added `unwrap()`/`expect()` hits are inside `#[cfg(test)]`, `tests/`, or `telemetry/testing.rs` (assertion helpers). | HOLDS (justified-invariant) |
| Ignored results (`let _ =`, `.ok()`) do not drop a security/accounting decision | 21 reviewed `let _ =` sites. Security/accounting-relevant ones: `session.rs:2963-2964,3049` (`sync_data`/`remove_file` on the disposable journal — documented recovery aid), `agent.rs:7134,7294-7296` (journal begin/append best-effort; the authoritative assistant entry is still written or the turn errors), `delegation.rs:2120` (provenance journal append after the roster was persisted; roster failures are stored in `state.persistence_error`, `delegation.rs:1940-1946`), `find.rs:77,90-91` (child kill/wait best-effort after output is collected), `responses_ws.rs:1484,1503,1536,1539,1563,1571` (progress/result channel sends after receiver cancellation), `cli/eval.rs:656-694` (loopback eval fixture only). None ignores a failed *policy*, *persist*, or *refusal*. | HOLDS |
| `unwrap_or_default()` does not convert "unknown" into "safe/zero" on a decidable path | 15 reviewed sites. `telemetry/schema.rs:145,160` + `spans.rs:193` are observation-only fallbacks; `proxy.rs:129` is guarded by the `host_str().is_none() → None` return above; `constrained_sampling.rs:385` treats an absent `required` list as empty (schema semantics); `powershell.rs:30` empty PATH → command lookup failure; `delegation.rs:4820` mirrors a previously-null cost as zero and immediately records the new one; `tools/durability.rs:437` is an inspection API where "no invocation" legitimately means no values; the rest are TUI/LaTeX/mermaid presentation fallbacks. No case turns a failed auth/cache/validation into a success. | HOLDS |
| No reachable panic-on-unknown in new production code | Two added `unreachable!` sites survive the test filter: `tools/deferred.rs:337` `(None, None)` is impossible because a missing handle immediately produces `Some(DeferredHandleRejection::Absent)` (`:311-314`); `cli/config_diagnostics.rs:148` covers the compiled-in config schema (loud panic if a future field is added without a mapping, never a silent pass). `assistant_frame.rs:399` `unreachable!("handled above")` is a transform-internal invariant verifiable from the match above it (`:380-390`). | HOLDS |
### Surface 9 — startup readiness / AWS discovery correctness (added by root mid-audit)

State at audit time: `crates/octet-coding-agent/src/providers/auth.rs` carried **uncommitted** `+277/-10`
(ai11's AWS metadata activation rule); `app/bootstrap.rs` was still clean at HEAD (wiring11's catalog/deadline
changes were **not present**), and `crates/octet-coding-agent/src/auth/codex/{resolver,store}.rs` were clean.
The binary under test was rebuilt from that worktree at 14:13:19Z. All probes used an isolated `HOME`, a scrubbed
`env -i`-style environment, `AWS_REGION=us-east-1`, and a local Python stub server on `127.0.0.1` as the metadata
target, so no real cloud endpoint was contacted. IMDS `http://169.254.169.254/` is unreachable here (curl: 3.006s
timeout), so "probe attempted" was decided by stub hits, not timing.

| Property | Evidence | Verdict |
| --- | --- | --- |
| No fabricated readiness | `declaration_is_configured` (`bootstrap.rs:3000-3026`) + `register_configured_presets_parallel` (`:3030-3080`) spawn one job per configured provider, `join` every one, and print a per-provider warning on error/panic; no provider is marked present without its own job succeeding. `doctor.rs:16` builds the real catalog and `:100` reports "configured model … is not visible". | HOLDS |
| Cache identity is bound to the credential/account | Codex: version + `cache.account_id != claims.account_id` + plan key + non-empty refuses (`bootstrap.rs:4697-4703`). Provider inventory: version + provider_id + inventory_url + `credential_fingerprint` (`:547-568`), where the fingerprint is `sha256(credential)` (`:457-459`, `:488-496`). Custom: version + base_url + credential fingerprint (`:3105-3112`), with store tests `provider_model_caches_are_isolated_and_deleted_together` / `model_cache_is_private_bounded_and_deleted_with_credential` (`auth/custom/store.rs`; not executed here). | HOLDS (by reading); custom-store tests UNVERIFIED (lib test target blocked by other workers) |
| Freshness is fail-closed on a clock rollback / future timestamp | `cache_modified_is_stale` → `modified.elapsed().map_or(true, ...)` treats unmeasurable age as stale (`bootstrap.rs:595-601`), used for provider inventory (`:602-608`) and Codex (`auth/codex/store.rs:331`, `:19`). | HOLDS |
| Stale dynamic Codex capabilities are not advertised as validated | Online: only a ≤1h, account- and plan-matched cache is authoritative (`bootstrap.rs:4972-4993`). Offline: a cached stale inventory is reduced by `conservative_offline_codex_models` (`:4735-4747`) — `responses_lite=false`, `agent_delegation=None`, ultra reasoning stripped — and an absent cache uses `fallback_codex_models(plan)` (`:4978-4988`). | HOLDS |
| Refresh-lock integrity (highest-risk item) | Today: `CodexResolver::load_valid` awaits `spawn_blocking(lock_store.lock_refresh())` **with no deadline** and keeps the JoinHandle (`resolver.rs:48-52`); the flock is released by RAII `Drop`/`finish` (`store.rs:84-110`) and the credential is re-read and re-validated under the lock (`resolver.rs:53-80`). No worker can be leaked in this state *because* the future is awaited to completion; the cost is an unbounded wait on a stuck flock. The planned bounded deadline is UNVERIFIED (not in the worktree). Invariant for that change: a `tokio::time::timeout` must NOT drop the `JoinHandle` — the blocking task stays parked in `flock` and the in-process `refresh_lock` mutex is released, so the next resolve spawns a second parked worker; use a deadline-aware acquisition (`try_lock_exclusive` + bounded retry inside one blocking closure, or keep the handle and await it after the timeout) and prove the lock is released on timeout. | HOLDS today; bounded-deadline change UNVERIFIED with precondition "wiring11's change not present" |
| Bedrock metadata is not globally disabled | Static env (`auth.rs:292-311`) and profile static credentials (`:313-338`) are checked **before** metadata and are untouched by the rule. Stub probe P1: `AWS_PROFILE=probe` with `~/.aws/credentials [probe] aws_access_key_id/aws_secret_access_key` → Bedrock registered in 28ms with **zero** metadata requests (`hits=[]`). | HOLDS |
| A positive indication still activates the probe (the standard opt-in, the product opt-in, the endpoint, container URI) | Stub probes (each saw the full IMDSv2 flow: `PUT /latest/api/token`, `GET …/security-credentials/`, `GET …/security-credentials/probe-role`): S2b `AWS_METADATA_SERVICE_ENDPOINT=<stub>`, S4b `AWS_EC2_METADATA_DISABLED=false` + stub, S5b `OCTET_AWS_METADATA_CREDENTIALS=1` + stub. S1b `AWS_CONTAINER_CREDENTIALS_FULL_URI=http://localhost:<stub>/creds` → `GET /creds` with `Authorization: <container token>`. The default endpoint `169.254.169.254` is attempted when indicated (all probe runs completed in ≤30ms only because the link-local route fails immediately here). Activation rule itself: `auth.rs:618-673`; the source includes dedicated tests (`unrelated_provider_launch_makes_zero_aws_metadata_requests`, `indicated_metadata_probe_reaches_the_ec2_source`, `indicated_metadata_probe_prefers_the_container_uri`, `an_unavailable_metadata_endpoint_costs_one_bounded_request`, `aws_metadata_service_endpoint_override_is_validated_fail_closed`) which could not be executed (lib test target broken by other workers). | HOLDS by probe |
| Explicit off / unknown value stays closed | Stub probe S7 `OCTET_AWS_METADATA_CREDENTIALS=0` + endpoint → `hits=[]`; S8 `AWS_EC2_METADATA_DISABLED=typo` + endpoint → `hits=[]` (both 2 doctor issues: Bedrock absent). `auth.rs:621-629`, `:631-647`. | HOLDS |
| **VIOLATED** — a profile-pinned IMDS endpoint enables the probe but is never used as the endpoint | `auth.rs:657-659` treats `profile_metadata_service_endpoint` as an *indication* only; `aws_metadata_service_endpoint_with` (`:720-728`) reads only `AWS_METADATA_SERVICE_ENDPOINT` and falls back to the `169.254.169.254` const. Probe P2: `~/.aws/config [profile probe] ec2_metadata_service_endpoint = http://127.0.0.1:<stub>/`, `AWS_PROFILE=probe` → stub `hits=[]`, Bedrock absent (2 issues), while the same value as an env var hits the stub (S2b). A user who pins IMDS in the standard profile key gets a probe to the wrong host and silently no credentials. Fix: when the env override is unset, use the validated profile value in `aws_metadata_service_endpoint_with`. | VIOLATED |
| **VIOLATED** — the standard AWS endpoint env var is not recognized | `rg -n 'AWS_EC2_METADATA_SERVICE_ENDPOINT' crates docs` → 0 hits; the code reads `AWS_METADATA_SERVICE_ENDPOINT` / `_MODE` (`auth.rs:703-704`, `:724`). `AWS_EC2_METADATA_DISABLED` (standard) *is* honored, so users reasonably expect the standard endpoint variable to work; instead the probe goes to the default host (fail-closed, no widening, but nonstandard and undiscoverable). Fix: accept `AWS_EC2_METADATA_SERVICE_ENDPOINT` / `AWS_EC2_METADATA_SERVICE_ENDPOINT_MODE` as the primary names. | VIOLATED (naming/nonstandard, fail-closed) |
| **VIOLATED** — the new rule is not documented for users | `docs/providers.md:119-120` still says Bedrock accepts "ECS/EC2 instance metadata" with no mention of the required local indication; `OCTET_AWS_METADATA_CREDENTIALS` appears **only** in `auth.rs` (`rg` → `:539`, `:557`, `:601`) and nowhere under `docs/`. A plain EC2 instance-role user (no `AWS_CONTAINER_*`, no `AWS_METADATA_SERVICE_ENDPOINT*`, no profile `credential_source`) now resolves no Bedrock models and sees only `Unknown model: bedrock/…` at launch, with no pointer to the opt-in. Fix: document the indication list + opt-in in `docs/providers.md`, and name the opt-in in the Bedrock-unavailable diagnostic. | VIOLATED (documentation/actionability) |
| Profile `credential_source` location | `auth.rs:691` reads the *credentials* file (`read_profile(false)`) and `:706` takes `credential_source` from it; the shared config file is read separately for `region`/`ec2_metadata_service_endpoint` (`:692`, `:707`). AWS documents `credential_source` for config-file profiles. The consequence (a config-file `credential_source = Ec2InstanceMetadata` not enabling the probe) could not be isolated because the default endpoint fails immediately here and the profile endpoint is ignored (previous row). | UNVERIFIED (missing precondition: a reachable default IMDS) |
| Unrelated providers cannot delay the selected route | Not true at HEAD: `register_configured_presets_parallel` joins every configured provider with no aggregate deadline (`bootstrap.rs:3067-3079`); per-request timeouts bound each request, not the join. Correctness is unaffected (fail-closed: the selected route is resolved from the full catalog; a slow provider only delays startup), but the property is violated as a latency guarantee. wiring11's planned per-job deadline is UNVERIFIED (not in the worktree). | VIOLATED (latency only; no fail-open) |
| A selected route never loses required discovery | The full catalog is built regardless of `--model` (`doctor.rs:16`, `cli/parity.rs:456`, `modes/interactive.rs:1372`, `batch.rs:244`); an unavailable selected model opens the picker with "selected model is unavailable; select a configured model" (`bootstrap.rs:5784`) or fails as `UnknownModel`. Compact/delegated/model-less edge cases were not individually probed. | HOLDS (core); edge cases UNVERIFIED |
| Offline / no-network behaviour | `env -i … octet --offline doctor` → 6-7ms, exits with the honest issues list, no hang; Codex offline uses the conservative reduction above. A missing-but-required Bedrock route fails with `UnknownModel` rather than hanging (see the documentation row for actionability). | HOLDS (bounded), actionability gap recorded above |
| No credential leakage in the new paths | Probes P3/P4/P5 embedded `SUPERSECRET` in `AWS_METADATA_SERVICE_ENDPOINT` (query), `AWS_CONTAINER_CREDENTIALS_FULL_URI`, and `AWS_EC2_METADATA_DISABLED`; stdout+stderr contained it **0** times (`secret-in-output=False`), errors are fixed strings ("invalid AWS metadata service endpoint", "invalid AWS_CONTAINER_CREDENTIALS_FULL_URI"). The container authorization token was delivered as an HTTP header to the stub and never printed. Metadata responses are capped (`MAX_AWS_METADATA_BYTES`, `auth.rs:879-887`), the client is `no_proxy` + no-redirect with 1s connect/total timeout (`:765-773`). | HOLDS |
| `activate_eager` is not serial / extension-startup claim | `activate_eager` builds one future per eligible extension and ends `join_all(requests).await` (`extension_runtime.rs:2432`), so a claimed "serialize → parallelize extensions" win is VERIFIED-FALSE. The startup status label is still `"starting extensions…"` (`modes/interactive.rs:6248`) although the wrapped closure also composes instructions and builds the App — it over-claims scope and defeats timing attribution; unchanged at audit time. | HOLDS (parallelization claim false); label observation VIOLATED (reporting only) |

Ranked VIOLATED items for this surface:
1. **Profile-pinned IMDS endpoint ignored** (probe P2). Fail-closed but breaks genuine metadata users. Minimal repro: `.aws/config [profile probe] ec2_metadata_service_endpoint = http://127.0.0.1:<stub>/`, `AWS_PROFILE=probe`, run `octet doctor` → stub sees nothing. Smallest fix: validate and use the profile endpoint when the env override is unset.
2. **Nonstandard `AWS_METADATA_SERVICE_ENDPOINT*` env names** (standard `AWS_EC2_METADATA_SERVICE_ENDPOINT*` recognized nowhere). Smallest fix: accept both names.
3. **Undocumented activation rule / non-actionable Bedrock-absent diagnostic** (`docs/providers.md:119-120` vs `auth.rs:531-539`). Smallest fix: document the opt-in and mention it in the per-provider warning.
4. **No aggregate deadline while joining all configured providers** (`bootstrap.rs:3067-3079`) — latency only; wiring11's change not present to audit.
5. **Status label over-claims** (`interactive.rs:6248`).

## Ranked findings (all surfaces) and residual unknowns

Confirmed VIOLATED findings, highest first:
1. Surface 9 #1 — profile-pinned IMDS endpoint enables the probe but is not used (fail-closed, breaks a genuine Bedrock
   metadata user; probe P2 above).
2. Surface 9 #2 — standard AWS endpoint env names not honored; only the octet-specific names work.
3. Surface 9 #3 — the new activation rule is not documented and the Bedrock-absent warning is not actionable.
4. Surface 9 #4 — `register_configured_presets_parallel` joins every configured provider without an aggregate deadline
   (latency, not fail-open); #5 — the "starting extensions…" label over-claims its scope.

Non-violation observations with smallest fixes:
- Surface 7: override entitlement uses the static table, not the discovered advertised max; bound it with
  `entitled_max.min(discovered_max.max(1))`.
- Surface 8: `responses_ws.rs:1344` can send an empty replay frame when there is no replay window (bounded by the
  3-attempt/6s budget); skip the send when `replay_frame.is_none()`.
- Surface 8: `cli/config_diagnostics.rs:148` is a loud `unreachable!` for a future schema field.
- Surfaces 1–8: no VIOLATED finding; the properties probed held (see tables).

Residual unknowns (each with the missing precondition):
- **Surface 1 / tmux**: herdr live path remains unprobed (`herdr` not installed); confirmed by reading only (predecessor's
  note, unchanged).
- **Surface 2 end-to-end CLI**: `crates/octet-coding-agent/tests/delegated_session_resume.rs` is an **untracked in-flight
  file by another worker** (not in `df5a7e80..HEAD`); all 5 of its tests currently fail in its own fixture at
  `delegated_session_resume.rs:252` ("one workspace-scoped store directory") because the committed goal store also
  creates `<session-dir>/.serve` (`app/bootstrap.rs:5969`). Store-level and resolver-level properties pass
  (`cargo test -p octet-coding-agent --lib -- worker_handle` → 3 passed), but the binary-level resume of an
  `agent-session:` handle is UNVERIFIED until that fixture is fixed by its owner.
- **Surface 3**: `theme/select` still has no host dispatcher arm (predecessor's observation); policy is only
  provable at the library level.
- **Surface 5**: the TUI attachment event path (composer/paste) was read, not driven under a PTY; `UNVERIFIED`.
- **Surface 9**: wiring11's catalog/deadline changes were absent from the worktree at audit time; the refresh-lock
  deadline and the per-job deadline are UNVERIFIED with the stated invariants. `providers::auth` unit tests and the
  custom-store cache tests could not be executed (the `octet-coding-agent` lib test target was broken by other
  workers' in-flight edits at audit time: first `tui/view/reasoning_render.rs`, later
  `AgentEvent::UserMessage`/`FinishReason`/`DelegationTelemetrySnapshot`); the stub probes above are the primary
  evidence for Surface 9. The profile `credential_source` location remains UNVERIFIED for lack of a reachable IMDS.

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


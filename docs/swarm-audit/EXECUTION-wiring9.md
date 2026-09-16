START 2026-09-15T17:42:55Z wiring9 alive
- 2026-09-15T17:42Z wiring9 alive: mapped the gap. Confirmed `resolve_launchable_child_session`
  lives in `pub mod delegation` (crates/octet-agent/src/lib.rs:74) so the CLI can call it without
  touching octet-agent. Confirmed `path_by_id` is the single choke point for `--resume <id>` in both
  print (bootstrap.rs:5739) and interactive (bootstrap.rs:5613) launches, plus cli/parity.rs:210.
  Delegation root is `<session-dir>/<workspace-key>/.delegation` (bootstrap.rs:6031).
START 2026-09-15T17:53:04Z wiring10 alive
START 2026-09-15T17:55:34Z wiring11 alive

## wiring11 (2026-09-15T18:20Z) — wiring9: launchable child handle wired into --resume

Adopted the dead predecessor's session_store.rs work (its one E0283 was already fixed by the
parent) and finished the job. `cargo check --workspace --all-targets --locked` re-run green after
my change; the only intervening red was another worker's in-flight edit to
`src/tui/view/reasoning_render.rs` (not mine, they since fixed it).

WIRED (crates/octet-coding-agent/src/session_store.rs, mine)
- `SessionStore::path_by_id` recognises the reserved `agent-session:` prefix and resolves through
  `octet_agent::delegation::resolve_launchable_child_session(self.dir.join(".delegation"), handle)`.
  `self.dir` IS the delegation root the host uses (`bootstrap.rs:6031`
  `session_parent.join(".delegation")`; `SessionStore::new` = `<session-dir>/<workspace-key>`).
  Both `--resume` choke points already go through `path_by_id` (bootstrap.rs:5626 interactive,
  bootstrap.rs:5739 print, cli/parity.rs:210 for `--name`), so no bootstrap edit was needed for the
  wiring itself.
- Token validated BEFORE any filesystem work (`delegated_handle_digest`: exact prefix + 64 lowercase
  hex; `.`/`/`/control/metacharacter can never reach a path join).
- Refusals are typed (`DelegatedHandleRefusal`, downcast-able through the `anyhow` boundary) with
  stable codes: `malformed_worker_handle`, `delegation_roster_unavailable`, `unknown_worker_handle`,
  `worker_awaiting_approval`, `worker_live_in_owning_process`, `worker_transcript_missing`,
  `worker_handle_outside_delegation`. Distinct, bounded (<=400 bytes), no path/credential echo.
- Live-in-owning-process: the durable roster cannot carry `record.live_task`, so a `pending`/
  `running` roster state is the fail-closed signal. Consistent with the in-process host verdict
  (`launchability` refuses `live_task`); a crashed run's stale `running` record is rewritten as
  `detached` when the owning session reopens, so the window self-heals.
- Confinement (`confine_delegated_session_path`): must be exactly `<delegation>/team-*/<child>.jsonl`,
  no symlinked team dir or child. Necessary because `delegated_session_reference` hashes ONLY the two
  trailing components (delegation.rs:142, its own test asserts same-leaf-elsewhere == same handle),
  so a forged/copied roster entry can name the same pair outside the delegation directory.

CLI-LEVEL PROOF (crates/octet-coding-agent/tests/delegated_session_resume.rs, NEW, 5 tests, all pass)
Real `octet` process + isolated HOME/workspace/session-dir + loopback provider; proves the handle
resolves (via `sessions inspect <handle>` -> `Path: <child>`), that `--resume <handle> --print`
appends the CHILD transcript while the parent is byte-identical AND replays the child's own history
(request contains the child-only marker, not the parent's), that a parked worker refuses with
"approval boundary"/"unattended mutation" without contacting the model or mutating the transcript,
that live/vanished/unknown/missing-roster/malformed/forged-escape each refuse distinctly, that a
malformed handle is refused with NO delegation directory on disk (validation precedes FS work), that
a forged entry cannot escape (same handle, refused as outside the delegation directory), that an
env secret + credential-shaped api_key + roster `durable_diagnostic` never appear in any output or
handle, and that ordinary `--resume <id>` + the picker (flat, non-recursive `candidates()`) are
unchanged.

OBSERVED
- `cargo test -p octet-coding-agent --lib session_store::tests` -> 40 passed; 0 failed
  (4 new: launchable resolution, distinct bounded refusals, malformed-before-FS, forged escape).
- `cargo test -p octet-coding-agent --test delegated_session_resume` -> 5 passed; 0 failed.
- `cargo test -p octet-coding-agent --test parity_cli` -> 16 passed; 0 failed (unchanged).

REQUIRED EXTENSION-SIDE CHANGE (not mine to make; extensions/octet-subagents belongs to openall4)
`launcher.py::resolve_worker_pane` hardcodes `resolvable=False` + `WORKER_PANE_BLOCKED_REASON`
(launcher.py:303-313) and `plan_open_all` plans only `worker.active` rows (launcher.py:387). Now that
`octet --resume agent-session:<sha256>` resolves, the extension must:
1. capture the host row's `launchable` (bool) and `launch_blocked` (str) in
   `orchestrator.py` where the `agent/list` record is folded into `Worker` (orchestrator.py:1303
   already reads `session` there) — the model currently drops both;
2. set `resolvable=bool(launchable)` and `blocked_reason=launch_blocked or None` in
   `resolve_worker_pane`, and plan a pane for every host-launchable worker rather than only
   `active` ones (a live worker is NOT launchable: one writer per session);
3. update the tests that pin the blocked worker pane
   (`tests/test_launcher.py:~412`, `tests/test_orchestrator.py` open-all expectations) and delete the
   now-false `WORKER_PANE_BLOCKED_REASON` text.
Also recorded in docs/subagents.md (open-all bullets + the worker-pane section).
START 2026-09-15T18:21:34Z wiring12 alive

START 2026-09-15T18:29:30Z wiring12b alive

START 2026-09-15T19:02:45Z wiring12c alive

START 2026-09-15T21:37:58Z wiring12d alive
- 2026-09-15T21:38Z wiring12d alive: plan = (1) bootstrap startup readiness split
  (availability -> route selection -> route init -> optional catalog enrichment),
  (2) Codex model note off startup (lazy first-turn/on-demand), (3) verify + finish the
  launchable-child-handle wiring that wiring11 recorded as done (re-run its tests).

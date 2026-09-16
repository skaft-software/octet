# Serve library regression repair

## Summary

Two targeted repairs are ready for parent-run verification. Only `crates/octet-coding-agent/src/extensions/serve.rs` and this report were edited. No trust/auth policy, delegation ownership, core session resolver, production GitHub timeout, or process-cleanup policy was changed. No cross-scope repair is needed for these two failures.

## Evidence and root causes

### Delegated inspector export: real adapter regression

The original panic at `serve.rs:11834` was **export**, not opening the inspector (`/tmp/octet-final/coding-lib-final.log:2336–2339`). The inspector had already opened and streamed the appended child turn.

`SessionStore::path_by_id` now interprets `agent-session:` identifiers as roster-resolved launchable workers (`crates/octet-coding-agent/src/session_store.rs:2570`). The Serve exporter copied its already-authorized child snapshot into a temporary flat session store using that same identifier, then called the ordinary exporter. The temporary store deliberately has no delegation roster, so lookup returned `NotFound`. Adding a fleet record or weakening launchability checks would incorrectly conflate read-only inspection with acquiring a child writer.

`serve.rs:2187–2213` now stages the snapshot under an ordinary temporary ID, runs the existing validating/redacting portable exporter, restores only the validated path-free public `source_id`, and rechecks the final serialized byte limit. The original descriptor identity/shared-lock checks and private temporary cleanup remain intact.

The existing inspector regression now additionally checks:

- public source identity and credential redaction;
- successful inspection/export without any launchable roster;
- raw snapshot and final serialized output byte limits;
- unchanged child transcript and removal of export temporary directories;
- export refusal for foreign resource-owner and missing-principal provenance.

The existing locked/read-only, live streaming, no-path-disclosure, empty command discovery, and command-refusal assertions remain (`serve.rs:11743–11947`, new export assertions begin at `11848`).

### GitHub descendant timeout: fixture readiness race

The original missing `descendant.pid` panic occurred after a 500ms query timeout (`/tmp/octet-final/coding-lib-final.log:2341–2344`). The fixture assumed the shell would start and publish its descendant before that deadline. Publication by the outer shell also did not establish that the child had installed its TERM-ignore trap.

The test at `serve.rs:13413` now polls the query once to start its owned process, then keeps that local future unpolled until the descendant itself publishes its PID **after installing its trap**. Readiness has a separate 15-second wall-clock watchdog. The real 500ms query timer is allowed to expire before resuming the query; production timeout and cleanup code are unchanged. Keeping the query local preserves process-tree drop cleanup on panic and avoids a detached task.

The test still asserts `Unavailable`, cleanup within one second after resuming the expired query, and descendant disappearance within two seconds. It additionally asserts the ready descendant is alive. The separate bounded-JSON/query test retains its prompt timeout-return assertion; this test no longer confuses executable startup latency with descendant cleanup latency.

## Verification observed

- `git diff --check -- crates/octet-coding-agent/src/extensions/serve.rs` passed; resulting diff reviewed.
- Extracted the exact embedded shell fixture with Python and passed `/bin/sh -n`.
- Three small Python-driven fixture probes passed on this host: readiness, survival of direct TERM after publication, and disappearance after owned-group KILL. Two ordinary startups published readiness around 0.456s/0.376s including a 50ms TERM probe; one deliberately delayed by 700ms published around 1.111s. These are shell lifecycle probes, **not Rust test execution**.
- No Cargo, rustc, Swift, build, or formatter was invoked. Rust compilation and both regression reruns remain unverified by this worker.

## Recommended parent verification

Retain the parent's low-disk build profile/environment:

```sh
cargo test -p octet-coding-agent --features serve --lib extensions::serve::tests::delegated_session_references_open_as_locked_path_free_inspectors -- --exact
cargo test -p octet-coding-agent --features serve --lib extensions::serve::tests::github_cli_query_timeout_kills_background_descendants -- --exact
cargo test -p octet-coding-agent --features serve --lib extensions::serve::tests::github_cli_query_accepts_only_successful_bounded_json -- --exact
cargo test -p octet-coding-agent --features serve --lib extensions::serve::tests::graphical_session_export_is_redacted_bounded_and_cleans_temporary_files -- --exact
cargo test -p octet-coding-agent --features serve --lib extensions::serve::tests::
```

Repeat the descendant regression under parallel library load after the focused pass. The other failures in the supplied library log remain outside this worker's scope.

## Artifacts / uncertainty

Artifacts: this report, the shared Serve diff, and the supplied `/tmp/octet-final/coding-lib-final.log`. No new octet artifact/session reference was exposed. Parent compilation and Rust behavioral results are required before claiming these regressions pass.

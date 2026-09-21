# API 0.3 current-candidate qualification

> **Historical API 0.3 qualification record.** The canonical example and its
> conformance tests remain live; these receipts do not qualify the current API
> 0.4 authoring path. The selected #253 gate is a bounded local tool smoke
> (negotiation, call, cancellation, shutdown), not Pi parity or every optional
> protocol service. See the [current guide](../extensions.md#bounded-authoring-path).

**Issue:** #253  
**Frozen qualification snapshot:** `71e2c317dc0559654423a9485ef3a2b7d76ab6e4` (read-only)  
**Prior authoring candidate:** `1dee2148c6af152e660007f7971c6f53372937af`  
**Repair status:** source-only, uncommitted; Rust host qualification pending

This record covers the bounded fixture-path and cancellation-synchronization repair. It is not an installed-candidate acceptance claim, a full unchanged Pi-parity claim, or live/physical qualification.

## Observed failure and cause

The frozen host command in `09-api-v03-runnable.log:2` compiled the fixture but failed before API negotiation. The test reported at lines 25–26:

```text
invalid file path: .../crates/octet-agent/../../examples/extensions/api-v03-minimal/extension.py
```

The fixture joined `CARGO_MANIFEST_DIR` with `../../examples/...` and passed that lexical path to the host. The explicit admission contract in `crates/octet-agent/src/secure_fs.rs:48-68` requires an absolute path with no `CurDir` or `ParentDir` component, and `open_regular_file_for_read` at lines 259–265 applies that validation. The transferred `extension_process.rs` entrypoint staging path calls this secure open at lines 10037–10045. The boundary is intentional and was not loosened.

The existing passing legacy Python-host fixture canonicalizes the same repository-relative path before using it (`crates/octet-agent/tests/extension_api_0_1_conformance.rs:274-283`). The repair follows that contract: `crates/octet-agent/tests/api_v03_runnable.rs:34-38` canonicalizes the example directory before joining `extension.toml`. Canonicalization resolves the filesystem's actual checkout spelling, so the fixture does not encode `/var` versus `/private/var`.

A follow-up repaired-stack run reached negotiation and the real call but failed at `11-api-v03-runnable.log:27-35` with `Err(Timeout { method: "tool/call" })` after 3.17 seconds. The fixture gave the request and shutdown drain equal three-second deadlines; `ExtensionProcess::shutdown` drains before its final shutdown stage (`crates/octet-agent/src/extension_process.rs:6683-6684`), so a fixed sleep did not prove that a pending request had been admitted and the request timeout could win the deadline race. The cancellation fixture now waits, with a bounded timeout, until `health_snapshot().pending_requests` is nonzero before shutdown, and gives the shutdown drain a one-second budget while retaining a three-second request timeout. This makes the expected `Cancelled { method: "tool/call", .. }` result distinct from `Timeout { method: "tool/call" }`.

## Scope and preserved behavior

The only repository source file changed in this repair is `crates/octet-agent/tests/api_v03_runnable.rs`; this qualification record is updated alongside it. The example directory was inspected but no example defect was demonstrated, so no example file changed. `extension_process.rs` remains Windows-owned and untouched.

The fixture's behavior assertions remain: API `0.3` manifest/version checks, required feature negotiation, the real `echo` call and structured result, shutdown-triggered cooperative cancellation, and clean process exit. The cancellation setup now waits for exactly one host request to be pending before invoking shutdown and retains the distinct `ExtensionRuntimeError::Cancelled { method, .. }` assertion; no production admission, negotiation, call, cancellation, or shutdown behavior was modified.

## Verification record

| Evidence | Result |
| --- | --- |
| Frozen `api_v03_runnable` host run (`09-api-v03-runnable.log`) | **Failed**, exit 101: startup rejected the non-canonical `ParentDir` path before negotiation. Retained as the motivating failure. |
| Repaired-stack `api_v03_runnable` run (`11-api-v03-runnable.log`) | **Failed**, exit 101 after 3.17 seconds: the real call reached its three-second timeout before shutdown produced host cancellation. Retained as the synchronization/timer-race failure. |
| Frozen broad rustfmt check (`13-rustfmt-changed-rust.log`) | **Failed**, exit 1: output reported formatting diffs beginning in the separately owned `crates/octet-agent/tests/recovery_current.rs` and other changed files. This is not a clean API-fixture formatting pass. |
| Repair-session Cargo/test/build/rustfmt checks | **Not run**; prohibited for this source-only owner lane. |

The prior candidate `RESULT.md` records pre-repair Python, SDK, generated-artifact, and static checks; those checks do not qualify the Rust host fixture after this repair.

## Exact handoff checks

After Luna integrates the uncommitted repair, `verify-rust` should run the focused host test with the admitted shared target and retain the prior failure record:

```sh
env -u OCTET_PACKAGE_DIR \
  CARGO_TARGET_DIR=/var/folders/d1/k5vl2s3n5nggpnfg963q1vwr0000gn/T/swarm-c88b634b2246.XI3ZIS/target \
  CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 \
  cargo test --locked -p octet-agent --test api_v03_runnable -- --nocapture
```

Then run the focused formatting check and record its exit independently:

```sh
rustfmt --edition 2021 --check crates/octet-agent/tests/api_v03_runnable.rs
```

The coordinator should inspect the integrated diff, confirm no `extension_process.rs` or unrelated owner paths changed, and record both commands against the integrated candidate rather than infer a pass from source inspection.

## Explicit limits

- No Cargo, rustc, Rust tests, build, or formatting command ran in this repair session.
- No installed binary or released-candidate acceptance was run.
- No live provider, credentials, remote service, physical terminal, PTY, or endurance evidence was gathered.
- No full unchanged Pi parity or issue-closure claim is made.

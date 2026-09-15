# #344 read-concurrency-current candidate qualification

**Status: source-only, uncommitted candidate.** This record describes deterministic fixtures and source inspection only. It does not claim Rust compilation, passing tests, runtime performance, live-provider/network acceptance, physical terminal behavior, installed-binary acceptance, or endurance.

## Candidate boundary

- Baseline: `e2eef46b051360600a06e72dc1694b4924c09c7c`.
- Owned production surface: `crates/octet-agent/src/agent.rs`.
- Owned fixture: `crates/octet-agent/tests/read_concurrency_current.rs`.
- Owned runtime entrypoints were inspected but intentionally unchanged: `crates/octet-coding-agent/src/main.rs` and `src/bin/octet-host.rs` retain explicit two-worker Tokio runtimes. No worker default was changed without measurements.
- Qualification uses local `tempfile` workspaces, deterministic Tokio barriers/notifications, and loopback `wiremock` SSE. It uses no provider credentials, live endpoints, remote actions, or copied Pi source.

## Deterministic fixture matrix

| Contract | Fixture evidence | Boundary |
| --- | --- | --- |
| Default `UnsafeHost` HostRead overlap, independent bounded waves, exact effect metadata, execution/result order | `host_read_waves_are_bounded_and_keep_effect_and_result_order`: six calls are held at a four-call barrier, then a two-call barrier; maximum active calls is asserted as four; policy effects, `ToolFinished` IDs, session entry count, and two provider requests are checked. | HostRead is represented by a trusted parallel probe; this is not filesystem or live-provider throughput evidence. |
| Read waves stop at an effectful mutation and hooks stay serialized | `read_waves_stop_at_mutation_barriers_and_serialize_hooks`: phase-one reads cannot release the mutation until both complete; phase-two reads cannot enter while mutation is held; started IDs, hook order, and hook non-overlap are checked. | Local in-process mutation probe under `UnsafeHost`; no external mutation is performed. |
| Every non-independent effect remains a barrier | `non_read_effects_are_ordered_barriers_without_parallel_admission`: an unknown call plus `WorkspaceMutation`, `HostProcess`, `Network`, `Delegation`, and `Extension` probes sit between two gated HostRead waves. The extension gate proves the second wave has not entered; policy effects, canonical started/finished IDs, and the unknown denial are checked. | Known effects are inert test probes under `UnsafeHost`; no process, network, delegated worker, or executable extension is started. |
| Cancellation, paired results, and no later model turn | `abort_during_a_read_wave_keeps_pairing_and_stops_future_turns`: both reads reach a barrier, `RunControl::abort` is sent only after the deterministic signal, both results are cancellation errors, the run is aborted, the session remains paired, and only the first request is received. | No timing sleep or provider cancellation claim. |
| Text/image/audio identity and canonical wire order | `text_image_and_audio_reads_retain_identity_in_ordered_slots`: gated built-in `read` calls consume local text, PNG, and WAV bytes; OpenAI tool IDs remain in model order and image/audio payloads retain type and relative base64 order. | Synthetic supported media and loopback wiremock only; media retention/request-size and live provider acceptance remain #343/live gates. |

The fixture also repairs the hook-overlap observation to use an `Arc<AtomicBool>` after the hook is moved into `ExtensionHost`, and removes an unused media-kind import. All fixture waits are barriers, one-shot signals, atomics, or notifications; no elapsed-time assertion is used.

## Source contract inspected

`agent.rs` keeps crash replay restricted to `Pure | WorkspaceRead` (`effect_is_repeatable_observation` and the recovery gate near `execute_recovery_call`). Live ordered waves use the separate `effect_is_parallel_observation` predicate, admitting only `Pure | WorkspaceRead | HostRead`; HostRead is not relabeled and still reaches the exact broker policy. `MAX_PARALLEL_READ_WAVE_WIDTH` is four, independent of Tokio worker count. Candidate calls are contiguous and model-ordered; every mutation, host process, delegation, extension, network, unknown, invalid, or sequential call is a barrier. Admission, before hooks, reservation commit, execution cancellation, deferred serialized after hooks, policy events, persistence, progress draining, and `ToolFinished` emission remain in call order.

The fixture source is intentionally registered through the public `Tool`, `ToolCallHook`, `ExtensionHost`, `EffectBroker`, `ReadTool`, and `RunControl` seams rather than reaching into private scheduler helpers. The existing `agent_run.rs:3392-3430` regression now uses a `Network` probe to retain the negative non-independent-effect barrier assertion; the dedicated fixture separately covers admitted HostRead waves.

## Verification record

No build, test, formatter, clippy, command, install, commit, or runtime execution was performed in this source-only lane. The following are **UNRUN** and must be executed by the centralized Rust verifier against the integrated snapshot:

```text
env -u OCTET_PACKAGE_DIR CARGO_TARGET_DIR=/var/folders/d1/k5vl2s3n5nggpnfg963q1vwr0000gn/T/swarm-c88b634b2246.XI3ZIS/target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 cargo test --locked -p octet-agent --test read_concurrency_current -- --nocapture
env -u OCTET_PACKAGE_DIR CARGO_TARGET_DIR=/var/folders/d1/k5vl2s3n5nggpnfg963q1vwr0000gn/T/swarm-c88b634b2246.XI3ZIS/target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 cargo test --locked -p octet-agent --test agent_run -- --nocapture
rustfmt --edition 2021 --check crates/octet-agent/src/agent.rs crates/octet-agent/tests/read_concurrency_current.rs
env -u OCTET_PACKAGE_DIR CARGO_TARGET_DIR=/var/folders/d1/k5vl2s3n5nggpnfg963q1vwr0000gn/T/swarm-c88b634b2246.XI3ZIS/target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=2 cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
```

The verifier must inspect the integrated diff and record any compile/format failures rather than infer a pass from fixture presence. No Cargo.lock edit is part of this candidate.

## Handoffs and remaining gates

- `crates/octet-agent/tests/agent_run.rs:3392-3430` is reconciled: `network_classification_overrides_a_parallel_tool_claim` uses `ToolEffect::Network` and `network-effect-sequential.jsonl` to retain the negative sequential-barrier assertion; admitted `HostRead` overlap remains covered by the dedicated fixture.
- `crates/octet-agent/src/tools/read.rs:233-236` is reconciled: the `concurrency()` comment now separates live `Pure`/`WorkspaceRead`/`HostRead` admission from exact `Pure`/`WorkspaceRead` crash replay; `Network` and other effects remain barriers.
- The two-worker runtime declarations in `main.rs:6` and `octet-host.rs:3` require measured qualification before any adaptive/default change: compare 2, 4, 8, and process-visible parallelism, including constrained CPU affinity, while measuring p50/p95 latency, control wake latency, CPU/RSS, OS threads, Tokio workers, and in-flight reads.
- Remaining acceptance gates are Rust compile/test/format/clippy, deterministic fixture execution, bounded-read performance/endurance/soak, physical TUI and control responsiveness, production smoke/runtime diagnostics, live provider and authorized remote-media/network qualification, and installed-candidate checks. Passing these source fixtures alone does not close #344 or #343.

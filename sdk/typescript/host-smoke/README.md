# Provider-free TypeScript authoring host smoke

This standalone unpublished probe uses the existing `octet-agent`, `tokio` and
`serde_json` source dependencies. It does not alter the root Cargo workspace or
introduce an SDK runtime dependency. It qualifies the current source Rust host,
not a released binary, provider turn, TUI screenshot or registry publication.

Prepare the [example's local package and generated manifest](../../../examples/extensions/typescript-hello/README.md), then from the repository root:

```console
cargo run --offline --manifest-path sdk/typescript/host-smoke/Cargo.toml
```

The probe asserts real manifest discovery and disabled-by-default start refusal,
explicit enablement, exact API `0.4` negotiation, useful `text_stats` dispatch,
malformed input refusal, cooperative cancellation followed by a usable same
process generation, and acknowledged host shutdown. Cancellation is generated
by dropping an active Rust host waiter, not manually faking the extension side
of the handshake. The smoke tightens the host cancellation grace to 300 ms
while work is delayed for 2 seconds and disables restart supervision, so an
uncancelled normal result or a replacement generation cannot mask a failure.
No providers, credentials, network or global installs are used.

## Typed source-SDK acceptance (no package preparation)

With Node >=22.19 available on `PATH`, run the focused integration target:

```console
cargo test --offline --manifest-path sdk/typescript/host-smoke/Cargo.toml --test native_typed -- --nocapture --test-threads=1
```

These nine `ts_native_*` tests invoke the source SDK CLI to generate a
manifest/launcher for the typed and native-breadth author fixtures, then use production
`ExtensionProcess` discovery, explicit enablement and dispatch. They require no
npm install, generated example files, provider or network. Each test owns a
private HOME/workspace and append-only child log (including PID), prints the
bounded log with `--nocapture`, and cleans its files on drop. Missing Node or
unsupported launch platforms fail rather than silently skipping.

Coverage is schema/structured-value/text roundtrip, omitted optional input and
explicit root null, invalid input with zero handler entries, invalid/nonfinite/
extra output with same-generation recovery, intentional domain failure, and
cooperative cancellation. Cancellation waits for an explicit child entry
barrier, drops the real host waiter, observes the host's terminal settlement,
then checks a valid follow-up and clean shutdown in the same PID/generation.
It does not infer cancellation from an arbitrary delay or allow supervision to
hide a replaced process.

Four added tests in `tests/native_breadth/mod.rs` use the author module
`tests/fixtures/native_breadth.mjs` (not a handwritten wire peer). They cover
resource export/resolution/release, foreign/unknown/retired zero-entry refusals,
failed-output provisional cleanup, failed disposers remaining retired, and
shutdown cleanup. Cleanup assertions wait for the production host's completed
or failed disposal status, not merely an author log. They also exercise a real
`policy/evaluate` reverse reply returning typed output with retained diagnostic
metadata, malformed diagnostic recovery, verified PNG publication and invalid
signature refusal, and a real before-prompt hook.

These four additions are authored but not native-run-qualified by the test
worker; the parent build/test run is required. No full Python/Rust parity,
resource race matrix, native bulk, private session hooks, provider media
projection, model-turn or installed-release acceptance is claimed. See the
[implementation/reachability/evidence table](../../conformance/sdk-parity.md).

The independent Node real-process/hostile-boundary/offline-pack suite is:

```console
npm --prefix sdk/typescript run test:process
```

Keep the generated canonical API `0.3` tests live separately. Do not retag them
as API `0.4`. See [the process runtime](../process/README.md) for authoring scope
and host bounds. The local lockfile pins the smoke's dependencies; target output
is ignored.

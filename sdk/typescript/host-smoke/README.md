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

The independent Node real-process/hostile-boundary/offline-pack suite is:

```console
npm --prefix sdk/typescript run test:process
```

Keep the generated canonical API `0.3` tests live separately. Do not retag them
as API `0.4`. See [the process runtime](../process/README.md) for authoring scope
and host bounds. The local lockfile pins the smoke's dependencies; target output
is ignored.

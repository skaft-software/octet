# `octet-serve` backend experiment

Maintainer reference for the optional graphical backend. For local use, start
with the [Serve guide](../../docs/experimental/octet-serve/README.md).
Serve remains experimental. This checkout's source distribution is 0.7.5;
public packages must match exactly. The [source notes](../../docs/releases/v0.7.5.md)
describe changes; the [exact GitHub release](https://github.com/skaft-software/octet/releases/tag/v0.7.5)
records publication and installation evidence. These implementation contracts do not
imply complete feature or live-provider acceptance.

## Focused checks

From the repository root, the independent package uses:

```console
cargo test --manifest-path extensions/octet-serve/Cargo.toml --profile ci-test --locked
cargo test -p octet-coding-agent --features serve
```

Omit `--profile ci-test` for Cargo's normal local test profile. The profiler
build command and profile settings are documented in
[build profiles](../../docs/build-profiles.md). The extension is deliberately
workspace-excluded, so its focused gate is additional to ordinary workspace
checks. Commands here document the checks; they do not report a new pass.

## Backend boundary

The package contains:

- bounded host, project, session, command, item, event, and replay DTOs;
- host-authoritative model, authority, capability, and compiled default theme catalogs;
- stable session cursors and exact durable entry identity;
- a deterministic session snapshot reducer;
- bounded replay plus device-scoped command idempotency;
- a host-scoped idempotent fresh-session operation;
- a serialized `SessionActor`;
- a `SessionSupervisor` that prevents duplicate mutable owners without holding
  its actor-map lock across slow session factories;
- a loopback-only HTTP/WebSocket transport with one-use launch authentication,
  strict same-origin checks, bounded requests, and safe static assets;
- an optional bounded in-process PTY manager for authenticated local terminal
  sessions; and
- `HostService` / `SessionDriver` adapter traits for the real octet application.

It does **not** contain a TUI, web layout, provider client, Agent, authenticated
LAN pairing, or a second session format. Bounded authenticated attachment and
content routes are implemented in the described snapshot. The feature-gated
first-party adapter in `octet-coding-agent` owns one existing `App` inside each
driver, translates real `AgentEvent` values into `TimestampedEvent`, and hydrates
committed `SessionItem` values from octet's append-only JSONL.

Golden JSON contracts for the browser/native client boundary live in
`fixtures/`. They use camel-case fields and explicit dotted command/event
discriminators. This package-specific graphical protocol is not an extension
API 0.3 authoring example or the separate native-host protocol 1.

## Local terminal

When the host's process-execution sandbox permission is enabled, the loopback
transport exposes an authenticated same-origin terminal WebSocket. It starts
shells only in the configured workspace, retains at most four sessions, and
uses an opaque owner key to reattach after a browser disconnect. Replay, input,
and terminal dimensions are bounded. Browser detach retains a shell; loopback
server shutdown stops every retained shell. This is lifecycle cleanup, not
process containment; see [lifecycle safety](../../docs/design/serve-lifecycle-safety.md#owned-subprocesses).

## Core adapter requirements

The feature-gated octet adapter must:

1. Create or open exactly one `App`/`Agent`/`Session` per driver.
2. Keep at most one active `Run` inside that driver.
3. Return immediately from `dispatch` after routing an admitted command.
4. Yield live agent events from `next_event`.
5. Include the exact durable octet `EntryId` in every committed item.
6. Treat model/reasoning/resume changes as idle-boundary operations.
7. Keep private confirmation senders inside the driver and expose only opaque
   public request IDs.
8. Never infer tool activity, sources, changes, or artifacts from model prose.
9. Scope idempotency keys by authenticated device identity.
10. Retain free-form one-shot answers only as non-reversible digests plus their
    nonsecret command shape, preserving exact idempotency without retaining
    plaintext.

## Package and release reference

The ordinary octet binary owns package management and a small external
`octet serve` dispatcher. The separately packaged feature-enabled runtime
contains the adapter into private `App`. Source-level extraction behind a stable
Runtime API is deferred; the default TUI, agent, AI, and `sexy-tui-rs` must not
depend on the web surface. See [architecture](../../docs/experimental/octet-serve/architecture.md).

The source package requires exactly octet `=0.7.5`. Use version-matched published
assets or a matching local build and reviewed local archive. See
[distribution channels](../../docs/distribution.md). It declares three targets:

- GNU/Linux x86_64: `x86_64-unknown-linux-gnu`;
- macOS x86_64;
- macOS arm64.

Linux musl is unsupported. Signed package publication and public installation
results belong to their version-pinned release record. A matching local archive
can also be installed offline. See
[package usage](../../docs/experimental/octet-serve/README.md#install-or-update-a-package).

The source-described `.github/workflows/release-serve.yml` contract accepts only
a finalized canonical stable `vMAJOR.MINOR.PATCH` release whose Cargo version
matches the tag. It builds optimized runtimes for the three targets, verifies
direct and package-dispatched launch, emits
`octet-serve-VERSION-TARGET.tar.gz`, writes SHA-256 checksums, signs archives and
the checksum manifest with keyless Sigstore bundles, and attaches them to that
existing canonical release. Repair/source tags use
`octet-serve-vMAJOR.MINOR.PATCH`; they do not replace the canonical octet tag.
`scripts/package-octet-serve-release.sh` is the local reproducibility and
package-layout gate before separately authorized publication.

[octet 0.7.4](../../docs/releases/v0.7.4.md) retains historical published-release evidence.
The earlier [octet 0.7.0](../../docs/releases/v0.7.0.md#release-verification) includes signed
Serve artifacts and verified public installation. Live-provider/native-audio checks
are optional and **NOT RUN**; package smoke does not establish full live-feature
acceptance. Earlier passes belong to the
[historical validation record](../../docs/experimental/octet-serve/current-state.md#validation-evidence).
Work tracking is on the [Project](https://github.com/orgs/skaft-software/projects/5).

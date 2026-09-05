# Experimental `octet serve`

`octet serve` is an optional first-party extension and application surface for
using octet through a shared graphical client. It is not a new interaction mode,
and it does not replace or mirror the terminal UI.

The first release target is a polished local web client backed by real,
headless octet sessions. A later gate adds accountless, mutually authenticated
LAN clients. Thin macOS, iOS, and Android shells follow only after the web and
LAN contracts are stable.

## Product contract

- Opening the graphical app at its root creates and selects a fresh provisional
  task in the focused work surface.
- Opening an explicit task route restores that task. Opening `/overview` loads
  the command center from session inventory without creating or opening a task;
  an already selected task remains selected in client state.
- Existing, pinned, and concurrently running tasks remain available in the
  sidebar.
- Each active task has one authoritative session owner and runs independently.
- The command center is a deterministic aggregate of host-owned task state. It
  surfaces exception counts, prioritizes tasks that need intervention or review,
  and supports task/project search without inventing summaries or runtime state.
- The transcript remains the primary surface for a focused task. Progress,
  sources, outputs, diffs, approvals, and previews appear only when real
  structured events create them.
- The interface has no Chat, Code, Work, or Cowork mode selector. The command
  center and focused task are two views of the same task lifecycle, not separate
  agent modes.
- `octet serve` is headless. It neither hosts nor synchronizes a TUI.

The graphical interaction grammar deliberately feels familiar to users of
ChatGPT and Claude/Cowork while remaining a clean-room octet implementation with
octet branding, terminology, themes, and security boundaries.

## Package boundary

Substantial implementation belongs outside octet's four core packages:

- `extensions/octet-serve/` owns the protocol, session service, transports,
  security, and embedded-asset host.
- `apps/web/` owns the shared React client.
- Later thin native applications live under `apps/`.

For this release, the package is binary-modular: the feature-enabled
runtime contains the smallest adapter needed to construct and control octet's
private `App`, while the ordinary octet binary owns only package management and a
small external `octet serve` dispatcher. Source-level extraction behind a stable
Runtime API is deferred. The default TUI, agent, AI, and `sexy-tui-rs` behavior
must not depend on the web surface.

The adapter and client are presentation-only boundaries. They must not add
presentation instructions to the model, alter the system prompt or active tool
schemas, insert frontend state into session content, or ask another model to
summarize work for the interface. octet's existing broad local authority remains
the default; client authentication and agent authority are separate controls.

See:

- [Current state and fresh-context handoff](current-state.md)
- [Architecture](architecture.md)
- [LAN pairing](lan-pairing.md)
- [Native delivery](native-delivery.md)
- [Web acceptance](web-acceptance.md)
- [Configured-provider acceptance](provider-acceptance.md)

## First web cut

The first complete vertical slice must use real sessions, not production
fixtures. It includes:

- a fresh-task launch flow and durable task sidebar;
- an exception-prioritized command center with aggregate status, task/project
  search, and direct return to focused work;
- two independently running sessions;
- streaming assistant, reasoning, tool, approval, and run-outcome items;
- stop, steer, and queued follow-up;
- model, reasoning, authority, and supported attachment controls;
- deterministic sources, outputs, changes, and previews;
- replay, idempotency, reconnect, and replay-gap recovery;
- responsive desktop, tablet, and phone layouts;
- the compiled default theme and canonical octet `01101111` byte mark.

Fixtures remain a development and test input only.

## Build, install, and release gates

With a locally qualified octet `0.7.0` build, use a matching local Serve archive
as shown below. Catalog installation requires separately verified publication;
the following commands document that conditional contract, not availability:

```console
octet extension install octet-serve
octet extension list
octet serve
```

`octet extension update octet-serve` reinstalls the package matching the running
octet version. `octet extension remove octet-serve` removes only package files and
preserves Serve sessions and other user data. A downloaded release archive can
be installed without network access to GitHub:

```console
octet extension install --path octet-serve-0.7.0-TARGET.tar.gz
```

The package requires exactly `=0.7.0` and supports GNU/Linux x86_64
(`x86_64-unknown-linux-gnu`) plus macOS x86_64/arm64. Linux musl targets are not
supported in this release. For development, run the embedded feature build
directly:

```console
cargo run --features serve -- serve
```

Because `extensions/octet-serve` is deliberately workspace-excluded, its focused
gate is mandatory in addition to the ordinary workspace gates:

```console
cargo test --manifest-path extensions/octet-serve/Cargo.toml
cargo test -p octet-coding-agent --features serve
```

`octet serve` binds IPv4 loopback only in this cut. A one-use launch capability
is exchanged for an ephemeral, HttpOnly, same-site browser cookie before any
API or event-stream access. This transport authentication is distinct from
octet's agent authority and from the future LAN device identity described in the
pairing plan.

The release workflow at `.github/workflows/release-serve.yml` accepts only a
finalized canonical stable `vMAJOR.MINOR.PATCH` release whose Cargo version
matches the tag. It builds optimized runtimes for the three supported targets,
verifies direct and package-dispatched launch, emits
`octet-serve-VERSION-TARGET.tar.gz`, writes SHA-256 checksums, signs the archives
and checksum manifest with keyless Sigstore bundles, and attaches them to that
existing canonical octet release. Repair/source tags use
`octet-serve-vMAJOR.MINOR.PATCH`; they do not replace the canonical octet tag.
`scripts/package-octet-serve-release.sh` remains the local reproducibility and
package-layout gate before any separately authorized publication. Declared
target support is not evidence that all platform acceptance gates have passed.

## Explicit exclusions

The first web release does not include MCP management, a plugin or extension
catalog and lifecycle UI, child-agent runtime trees, LSP, scheduling, TUI
synchronization, WAN access, multi-host replication, or a hosted account
service. Host-admitted skills, prompt templates, and enabled extension commands
are available through the composer; a bounded local PTY is available when host
authority permits process execution. Missing capabilities do not appear as
empty navigation or dashboard sections.

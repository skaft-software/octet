# Serve integration ownership split — qualification record

## Scope and status

This candidate records the Serve ownership split in the coding-agent crate. It is a source-only, uncommitted handoff record; no release or production qualification is claimed. The requested change keeps `extensions::serve::run(...)` as the existing entry point and exposes startup naming only through the Serve handoff (`run_with_session_name`).

## Ownership map

- `startup.rs`: process lock, loopback launch, browser handoff, signal shutdown, launch-option normalization, and startup session-name validation.
- `routing.rs`: `HostService for OctetHost`, capability advertisement, project/session admission, attachments/documents, and transport conversion.
- `projects.rs`: project registry and project-scoped state operations.
- `sessions.rs`: session catalog/seed/bootstrap helpers and session metadata operations.
- `conversations.rs`: conversation branch and attachment-facing domain helpers.
- `recovery.rs`: deletion journal and recovery helpers.
- `runs.rs`: worker plan/commands/messages and serialized worker execution, including admission, cancellation, retries, replay, edits, forks, rollback, model changes, metadata, and shutdown paths.
- `serve.rs`: composition-root types and the remaining adapter/driver/projection/resource/repository glue that still depends on the parent `App` boundary.

The old duplicate routing and worker definitions are disabled, so active ownership is single-sourced in the child modules while existing internal call sites retain their names.

## Preserved contracts

- Authority ceilings, capability/origin checks, project trust boundaries, and subprocess/disconnect handling remain unchanged.
- Cancellation, replay, retry, fork, rollback, model-change, and worker shutdown paths remain serialized through the existing worker boundary.
- A fresh Serve launch still creates and bootstraps a provisional session.
- Startup names are trimmed; empty names are ignored; control characters and names over 120 characters are rejected. Accepted names are persisted through `SessionStore::rename` before the first prompt and therefore use the normal metadata/reconnect path.

## Verification boundary

No commands, tests, builds, formatter runs, or dependency changes were performed for this candidate, per task constraints. The following checks remain **UNRUN**:

- `cargo fmt --all -- --check`
- `cargo test -p octet-coding-agent --features serve`
- the Serve ownership integration test/qualification matrix, including startup naming, routing admission, cancellation/replay, disconnect, and subprocess behavior

The existing inline Serve unit tests were not mechanically moved or rewritten without an executable validation pass. No standalone integration fixture was present in the source tree at handoff time.

## Remaining handoff boundary

The CLI dispatch in `crates/octet-coding-agent/src/lib.rs` still calls `extensions::serve::run(config, port, no_open, web_root)`. A caller that owns a startup session-name option must hand it to `run_with_session_name(config, port, no_open, web_root, session_name)`; the CLI parser remains outside this Serve-owned change.

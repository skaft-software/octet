# Octet Serve backend lifecycle and security qualification

**Candidate status:** source-only fixture/documentation revision; pending the independent workspace checks.

**Scope:** qualify the owned serve backend's lifecycle, ownership fencing, bounded reconnect replay, opaque resource handling, strict public DTOs, and sanitized errors. Provider-owned files (`extensions/octet-serve/src/transport.rs` and `extensions/octet-serve/src/lib.rs`) are intentionally out of scope.

## Deterministic qualification fixtures

| Fixture | Qualification coverage |
| --- | --- |
| `extensions/octet-serve/tests/lifecycle_current.rs` | Ten independent cores, duplicate command identity, cancellation isolation, bounded replay, and per-session projections. |
| `extensions/octet-serve/tests/lifecycle_full.rs` | Dropped client attachment without owner cancellation; exactly-once command effect; owner-loss fencing and shutdown; stale-generation rejection; cursor-bound replay gap/tail recovery. |
| `extensions/octet-serve/tests/security_full.rs` | Opaque session-bound resource handles; path-like display-name sanitization; durable reopen; immutable binding conflict; strict DTO unknown-field/size/generation validation; sanitized internal errors. |
| `extensions/octet-serve/src/actor.rs` internal tests | Owner replacement fencing, checkout boundaries, replay, approvals, command identity, and driver shutdown/quiescence invariants. |
| `extensions/octet-serve/src/supervisor.rs` internal tests | Session isolation, replacement fencing, catalog ordering, and retirement/quiescence behavior. |

The fixtures use fixed identifiers, bounded payloads, and temporary private state. No provider adapter, transport, network, credential, or filesystem path is exposed through a public assertion.

## Acceptance invariants

- A cloned actor handle is an attachment, not a second owner. Dropping one attachment does not stop the serialized driver; dropping the final handle permits shutdown.
- `ServiceError::OwnerLost` closes the actor before an acknowledgement is cached or a replacement owner can be admitted.
- Commands carrying a stale actor generation are rejected before dispatch and include the current generation for reconnect recovery.
- Replay is generation-bound and returns either the retained cursor tail or a bounded snapshot gap when retention has been exceeded.
- Resource content is resolved only through a valid opaque handle bound to the requesting session. Invalid or cross-session handles are indistinguishable from missing resources.
- Resource bytes are immutable and integrity-checked across a store reopen; conflicting reuse of a session/tool/slot binding fails closed.
- Public command DTOs reject unknown fields, oversized prompt text, and missing ownership generations.
- Internal service failures serialize as a stable generic error without private source details or control characters.

## Verification record

The repository inspection found no demonstrated production defect requiring a backend-source change for this candidate. The qualification fixtures and this record are additive only; provider-owned files remain unchanged.

The independent workspace checks are intentionally **not run in this task**. Before treating the candidate as passing, run the workspace's normal formatting, test, and check commands, then record their exact results here. Until then, the status is **pending / unverified**, not passing.

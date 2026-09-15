# Serve lifecycle current-candidate qualification

**Issues:** [#396](https://github.com/skaft-software/octet/issues/396),
[#341](https://github.com/skaft-software/octet/issues/341)

**Baseline:** `e2eef46b051360600a06e72dc1694b4924c09c7c`  
**Candidate revision:** pending; source-only and uncommitted

> **Observed run added:** [`serve-lifecycle-reconnect-full.md`](serve-lifecycle-reconnect-full.md)
> and [`serve-security-full.md`](serve-security-full.md) now record executed
> runs at `00e3ca3e` (both fixtures pass; `tests/lifecycle_full.rs` had a
> compile error that is fixed). The **UNRUN** cells below are retained as the
> pre-run snapshot of this source-only candidate; the live-host, browser, and
> measured-OS-budget gaps are unchanged.

## Scope and status

This is a bounded source candidate, not a release, beta, installed-candidate, or
live acceptance record. Serve remains experimental. Source inspection found no
demonstrated production defect, so no production implementation was changed.
The existing fixture edits are retained; this record does not infer a passing
check from their presence.

## Exact source changes

- `extensions/octet-serve/src/supervisor.rs:2690-2796` adds the existing
  `ten_sessions_isolate_attach_loss_duplicate_cancel_and_bounded_replay`
  Tokio fixture. It creates ten host-owned sessions, reopens each session and
  verifies the handles share one actor, drops one handle while an attachment
  remains, routes one prompt, checks exact duplicate-ack reuse and one mock
  dispatch, admits an explicit abort, checks per-session item isolation, and
  checks a two-event journal's snapshot gap and retained tail.
- `extensions/octet-serve/tests/lifecycle_current.rs:1-199` adds an external
  public-API fixture for ten independent `SessionActorCore` instances. It
  exercises duplicate admission, a working-to-stopped event sequence, per-core
  metadata isolation, and bounded gap/tail replay without the unit-test
  `MockHost`.
- No changes were made to production behavior in `actor.rs`, `transport.rs`,
  `runtime_status.rs`, or any other path by this candidate.

The narrow API/type/assertion inspection found the integration fixture's public
exports and constructor signatures available. Its seed satisfies the actor
identity/authority contract; `ActorConfig::default()` has a `FullAccess` ceiling;
event cursors are assigned by the actor; and the replay assertions correspond
to the journal's two-event retention rule. This is static source evidence only.

## Deterministic coverage selected

| Contract slice | Source coverage | Qualification state |
| --- | --- | --- |
| One mutable owner with attach/reopen | supervisor fixture and `open_session` seam | UNRUN |
| Ten-session snapshot/effect isolation | both fixtures | UNRUN |
| Duplicate command idempotence | exact ack and dispatch-count assertions | UNRUN |
| Explicit cancellation distinct from handle drop | abort admission plus retained attachment | UNRUN |
| Bounded replay fallback | old-cursor gap and current-cursor tail assertions | UNRUN |

The fixtures do not establish real process, file-descriptor, mailbox, output,
or dormant-session budgets; approval/steer/follow-up/pause behavior; transport
disconnect/reconnect, host loss, or takeover; durable storage restart; or
cross-client TUI parity. No defect is reproduced by source review.

## Proposed checks — all UNRUN

Rust verifier, from the repository root:

```console
cargo test --manifest-path extensions/octet-serve/Cargo.toml --profile ci-test --locked
cargo test --manifest-path extensions/octet-serve/Cargo.toml --profile ci-test --locked ten_sessions_isolate_attach_loss_duplicate_cancel_and_bounded_replay
cargo test --manifest-path extensions/octet-serve/Cargo.toml --profile ci-test --locked ten_core_sessions_keep_effects_isolated_and_replay_bounded
cargo test -p octet-coding-agent --features serve
cargo fmt --manifest-path extensions/octet-serve/Cargo.toml -- --check
```

Non-Rust/source verifier:

```console
git diff --check
```

The verifier should also inspect the integrated diff, package boundary, and
Markdown links against the exact candidate revision. No formatter, build, test,
or diff check was run here.

Browser/WebSocket verifier: against the exact built candidate, use a fresh
private workspace and no live provider credentials; launch the IPv4-loopback
host with an ephemeral port and exercise one-use launch exchange, same-origin
and cross-origin rejection, bounded snapshot/replay reconnect, duplicate
command delivery, client detach without implicit abort, explicit abort, and
ten concurrently visible isolated sessions. Record command, revision, exit,
resource bounds, and observations. This browser journey is **UNRUN**; source
fixtures do not substitute for it.

## Physical, live, and publication limits

No physical terminal/process-tree or PTY observation, native-client check,
live-provider journey, installed binary/package smoke, remote/LAN check, or
public release/publication action was performed. The source contract remains
IPv4-loopback-only and does not support LAN pairing; this record makes no
sandbox, security, release, beta, or public-installation claim. A separate
verifier must record unavailable cells rather than convert source inspection or
historical 0.7.4/0.7.6 material into current-candidate evidence.

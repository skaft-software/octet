# Bounded computer-use composition contract

Status: tested synthetic source composition, **not installed/native/security
qualification**. The manifest remains opt-in and unchanged. No live desktop,
provider, permission request or model-code worker is exercised by these tests.
See [README.md](README.md) for the exact implemented operation subset and
deterministic test commands.

## Integration and remaining mismatches

- `main.py` accepts an optional trusted-local `MacOSRuntime`. Standalone has no
  runtime, scoped policy adapter or factory, and remains inert. This is not a
  negotiated host feature: API 0.3 offers no automation policy/approval service.
- `runtime.py` translates the host owner triple and selected native target to
  the existing `LifecycleSession`. Policy `owner_id` is the host extension
  instance fence, and `extension_generation` is the host process generation.
  It verifies those mappings against the trusted scope before any factory call.
- Native app/PID/window/process-start identity and original observations are
  private. The lifecycle frame changes when the observed evidence digest
  changes, not on every recapture. Revalidation captures fresh native evidence
  both before policy and again before the one-use grant is consumed.
- Only the well-defined operation overlap is composed: observe, exact AX-center
  left click, navigation keypress, focused editable text and scroll. No blind
  coordinate approximation, target selection, app launch, unsupported action,
  screenshot projection or arbitrary native factory is accepted from the model.
- Windows ownership, native action sequences and screenshot artifact contracts
  remain independent modules. They are not interchangeable dataclasses and are
  not wired to this entry point by pretending their fields match.
- Exact local `evaluate_action` receives the full action binding, not the legacy
  `PolicyIntent`. The legacy API 0.2 intent shape cannot bind private arguments
  and observation/native identity. It is no longer sufficient to admit input.
- Code-runtime capability flags and AST validation are not OS containment.
  Primitive detection and caller-supplied sandbox objects cannot enable source
  execution. The unqualified fork launcher is removed; runtime and worker
  entry points return unavailable without executing model source.

## Authority and lifetime

A trusted embedding supplies enablement, the exact-action policy evaluator,
selected target, resource owner and extension generation. Tool arguments never
supply authority. Missing host API negotiation is not silently replaced with a
cooperative confirmation UI or a protocol downgrade. The embedding remains
responsible for active parent-request validation and for issuing a fresh owner
process generation after settlement; the extension cannot manufacture it.

One runtime has one selected owner/target and serial bounded operations. The
backend factory is called only after explicit host authorization of the initial
observation. Unknown operations/fields are rejected before policy or dispatch.
Mutations require retained observation/native identity digests, a matching scope,
an exact host decision, fresh target revalidation and a registered one-use grant.
Scope limits and expiry are checked at evaluation and dispatch. Denial, expiry,
stale frames, cancellation and changed identity dispatch no input. ASK tokens
are exact, short-lived and retry-bound; tokens appear only at the trusted policy
boundary, not in model output. Authentication and credentials remain manual.

Lost input acknowledgement or result transport means unknown effect: stop,
invalidate and never replay. Acknowledged input/reobservation is explicitly not
verified task success. The macOS backend rechecks permissions, foreground,
window identity/geometry and Accessibility content/focus after confirmation.
A native confirmation is an additional safety check, never host authority.
The backend invokes the exact one-use authorization callback after its final
checks, immediately before input; expiration during revalidation still denies.

Trusted stop/takeover, cancellation, settlement and process EOF/shutdown revoke
admission, cancel in-flight work, request input release and invalidate frames.
Late observations cannot repopulate settled state. A stopped backend is terminal.
Cleanup is bounded; missing/non-boolean/failed release acknowledgement or a stuck
native call is degraded, never proof of release or rollback. The retained native
macOS helper still has best-effort release without an explicit acknowledgement;
only the synthetic fixture presently provides the tested acknowledgement.
Physical takeover detection and hard process-loss release need native evidence.

## Screenshot and runtime boundaries

The independently tested screenshot module requires selected-window-only,
bounded pixel/byte/count storage under an owner/generation. Durable records hold
references, never payloads in receipts/logs; projection needs an explicit live
matching owner. Staged files have a per-transport owner prefix; cleanup never
sweeps another transport's in-flight screenshots from shared host scratch.
No arbitrary model path is a screenshot source. This contract
is not an assertion that API 0.3 projection or lifecycle wiring is implemented:
the composed runtime refuses screenshot operations and never captures payloads.

Ordinary forked Python inherits descriptors, credentials and process memory.
The namespace prototype neither moved the running worker into a new PID
namespace nor established descriptor/capability/syscall isolation. No shipped
launcher is qualified, including on Linux. Failed or unsupported containment
must execute no model source, even with all capability booleans set to true.

## Host integration blocker (unchanged)

The scoped authorization *inside* this extension is wired and fail-closed: the
entry point dispatches through `MacOSRuntime`, which owns the `LifecycleSession`
and the exact-action `PolicyGate`, and a created runtime refuses to exist unless
the trusted `Scope` already matches the selected owner generation, session,
target app/window and native identity. What remains unimplemented is the
**host-brokered adapter**: API 0.3 offers no typed automation
policy/approval/target-selection service, and the native protocol-1 host in
`crates/octet-coding-agent/src/host/policy.rs` deliberately never starts
executable extensions. The exact missing primitive is a negotiated API 0.3
capability (for example `policy/evaluate` plus a host-owned approval/target
selection service) that supplies `PolicyEvaluator.evaluate_action`, `Scope` and
owner/target settlement to this process. No Rust host authority was fabricated,
and no cooperative confirmation UI, native permission flag, model context or
configuration field is treated as that authority. Until then the standalone
process stays inert (no evaluator, no scope, no factory, no dispatcher).

## Stop, takeover and scope lifetime

`stop()`, `takeover()`, process EOF/shutdown, cancellation and lost response
transport are the only revocation entry points; they are not tool operations and
cannot be named by model arguments or host context fields. A stopped or replaced
binding is not reusable: the gate is terminally revoked and the lifecycle owner
is settled, so a later request is denied before policy evaluation, before the
native factory and before input, even when its scope identifier, frame
generation or captured grant is replayed. An unknown or replaced scope
identifier is denied at the same boundary.

## Verification boundary

Deterministic tests exercise the actual entrypoint, lifecycle, backend, policy
and existing screenshot code with synthetic native fixtures only. Tests do not
load native OS APIs, start code workers, contact providers or ask permissions.
They are not live macOS/Windows, sandbox-escape, host-frontend, package-install
or release-packaging qualification. Outstanding work is recorded explicitly;
no unrun test or platform check is reported as passed.

The stop/scope lifetime properties above are covered by
`tests/test_runtime.py::RuntimeTests::test_unknown_or_replaced_scope_identifier_dispatches_nothing`,
`::test_stopped_binding_cannot_redeem_a_captured_grant_or_frame` and
`::test_stop_and_takeover_are_trusted_entry_points_not_tool_arguments`, alongside
the existing policy/lifecycle/runtime suites.

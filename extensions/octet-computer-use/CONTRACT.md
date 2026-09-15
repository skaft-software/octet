# Bounded computer-use composition contract

Status: source integration, not installed/native/security qualification. No code,
tests, captures, UI, permission requests or native actions were run for this packet.
The manifest remains opt-in and unchanged. No provider credentials or model-name
inference are part of this extension.

## Seed inventory / mismatches

- `main.py` imports only `policy` and `protocol`; its dispatcher takes
  `(ActionRequest, arguments, threading.Event)`, not the lifecycle adapter shape.
- Policy target: kind/app/window/origin/display. Lifecycle target:
  app/window/tab/frame-generation. macOS target: bundle/PID/window/code identity.
  Windows target: explicit TargetSpec plus attached HWND/process-start/desktop/
  verified application identity. These are not interchangeable dataclasses.
- Policy uses `extension_generation`; lifecycle uses `process_generation`.
  macOS stores state by an owner key; Windows instances each own one attachment.
- Lifecycle calls `observe(owner, target, cancellation=..., timeout=...)` and
  `perform_action(owner, target, ActionSpec, ...)`. macOS instead takes target
  first, keyword owner, and `perform`; Windows takes typed InputAction sequences
  and millisecond timeouts. Cancellation shapes also differ.
- macOS observations contain AX tree/window geometry and optional Screenshot;
  Windows observations contain encoded image bytes and WindowIdentity. Neither
  is a lifecycle TargetObservation or a durable screenshot reference.
- Policy ASK/one-use grants are not lifecycle boolean approvals or macOS
  cooperative confirmation. A native safety confirmation never grants authority.
- The seed policy intent omits the exact action binding and the seed ledger keys
  only argument digest/frame. The broker must bind the actual observation and
  native identity, arguments, owner, request, scope and generation, including
  revalidation after policy and before input. No prompt or backend hint is allow.
- Code-runtime capability flags/AST validation are not proof of OS containment.
  Ordinary forked Python inherits descriptors, credentials and process memory;
  failed/unsupported containment must execute no model source.

## Authority and lifetime

A trusted embedding supplies enablement, the policy evaluator, exact target
selection, resource owner and extension generation. Tool arguments never select
native factories or supply host authority. Standalone startup remains inert and
fail-closed when those host dependencies are absent. Missing API negotiation is
not silently replaced with a cooperative confirmation UI.

One runtime has one selected owner/target and serial bounded operations. Native
modules are imported/constructed lazily after explicit host opt-in. The broker
retains native identity and observation evidence privately, not model-provided
copies. Every operation is typed and bounded; unknown operations/fields are
rejected before policy or dispatch. Mutations require the current observation,
an exact host decision, fresh target revalidation and a one-use grant. Denial,
expiry, stale frames, cancellation and changed identity dispatch no input.
Acknowledgement loss after dispatch is unknown effect: stop, invalidate and never
replay. Successful input alone is not verified task success.

Stop/takeover, cancellation, settlement and process EOF revoke admission, cancel
in-flight work, release input and invalidate all frames. Reattachment needs a new
runtime/owner generation and fresh observation. Cleanup is bounded; a stuck
native call is degraded, never evidence of rollback. Physical takeover detection
and hard process-loss input release require native qualification.

Screenshots are selected-window-only, bounded by pixels/encoded bytes/count and
owner/generation. Durable records contain references, never payloads in logs or
receipts. Projection requires explicit selection and a live matching owner;
stop/settlement revokes projection and cleans owner-local storage. No arbitrary
model path is a screenshot source.

## Verification boundary

Authored mocked-native composition tests exercise real broker/backend/policy/
screenshot code with synthetic fixtures only. They must not instantiate native
OS APIs, start a code worker, contact a provider or ask for permissions. They are
not live macOS/Windows, sandbox-escape or host-frontend qualification. Outstanding
qualification is recorded separately; no tests are reported as passed unrun.

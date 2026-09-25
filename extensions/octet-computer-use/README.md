# octet computer-use extension

Source-only API 0.3 entry point, scoped policy boundary, and a **trusted-local,
mocked-native macOS composition**. This is not an installed/native-qualified
computer-use product. Standalone startup remains inert.

This extension source manifest is pinned to Octet `0.8.1-rc.1` in the local
candidate checkout. Use `--extension-dir ./extensions` with the matching RC
binary; the published 0.8.0 bundles remain version-locked to the stable host.
`main.py` resolves those modules through the host-provided `OCTET_EXTENSION_DIR`,
falling back to its own directory only for direct source execution. Successful
API negotiation does not install a desktop backend or grant native-input authority.

Read [CONTRACT.md](CONTRACT.md) before embedding. The exact extension wire is
[API 0.3](../../docs/extensions/API-0.4-REFERENCE.md); capability ownership and
process trust remain governed by [extensions](../../docs/extensions.md) and
[security](../../SECURITY.md).

## Authority boundary

The manifest remains opt-in and declares no filesystem, process or network
capability. Those declarations are consent metadata, not OS containment.
API 0.3 currently has **no negotiated automation policy, approval, target-picker,
or trusted stop/takeover service**. The separate native protocol-1 controlled
host does not start executable extensions. This implementation does not invent
those services or downgrade to API 0.2.

A trusted embedding may supply `ComputerUseExtension(..., runtime=...)`, where
`MacOSRuntime` requires all of:

- explicit `enabled=True`;
- a host-derived `OwnerIdentity` (session, extension instance, process generation);
- an exact host-selected native bundle/PID/window/process-start identity;
- a `PolicyGate` with a matching, bounded, expiring `Scope`; and
- a lazy owner-local backend factory. There is no default native factory.

The gate requires a trusted local `evaluate_action(binding, *, parent_request_id,
approval_token)` implementation. **This is not a JSON-RPC method.** Legacy
`evaluate(intent)` adapters are refused: their intent omits exact arguments and
observation evidence. The embedding must derive active request identity and
approval decisions itself; the extension cannot qualify a supplied callback.

Bindings include the exact operation, capability/effect, owner, session, target,
origin where applicable, scope, process/frame generation, private argument
digest, observation digest and native-identity digest. Grants are registered
locally, short-lived, one-use and parent-request-bound. Scope expiry uses the
gate's monotonic millisecond clock (or the explicitly injected clock), not Unix
time. Revocation is terminal. ASK retries require the exact issued token and
binding; no prompt, native permission flag, or cooperative confirmation is allow.
Credential/authentication classes and protected native controls remain manual.
Provider credentials, headers, leases and transport authority never enter the
backend interface.

## Bounded composition

`runtime.py` connects the entry point, existing lifecycle, exact policy gate and
macOS backend for these operations only:

| Operation | Arguments / constraints |
| --- | --- |
| `observe` | `{}`; selected window, value-free bounded AX tree; no screenshot |
| `click` | `x` and `y`, or `coordinates: {x, y}`; left button; exactly one observed AX node center |
| `keypress` | One lifecycle navigation `key`; Arrow names map to native navigation keys |
| `type` | Bounded `text`; exactly one focused editable non-sensitive control |
| `scroll` | Bounded integer `delta_x`/`delta_y`; native restrictions still apply |

Coordinates are AX screen-space points, not screenshot pixels or an arbitrary
click surface. API 0.3 inputs are portable integers; fractional inspection values
are decimal text, never rounded input coordinates. Fractional window dimensions
are refused by this composition. Native identity and full observation evidence
stay private; model-returned copies never replace them. Input is revalidated
after policy, then the native backend rechecks permission, foreground, window
identity/geometry and AX content/focus after its separate confirmation boundary.
The final scoped one-use authorization callback runs after those native checks,
immediately before input, so time spent revalidating cannot extend a grant.

The entry point retains its source tool operation catalog. `start`,
`double_click`, `drag`, `move`, `wait` and `screenshot` are explicitly denied by
this composition; they are not silently approximated. The Windows backend and
screenshot artifact modules remain independent tested source, not integrated
production dispatch. API 0.3 media projection is deferred. No screenshot bytes,
arbitrary paths or model-code operations are admitted here.

## Lifetime and qualification

One runtime has one owner/target and serial bounded calls. Trusted local
`stop()`/`takeover()`, process EOF/shutdown, cancellation and lost response
transport revoke admission, settle the lifecycle, clear observations and request
input release. Stop/takeover are not model tools. Late observation completion
cannot revive settled evidence. Unknown input acknowledgement never permits
replay; successful input/reobservation is not verified task success.

Cleanup requires an explicit boolean acknowledgement. A missing, truthy-object,
failed or stuck release is degraded, not success or rollback. The retained
`MacOSNative.release_all` is best-effort and does not yet supply that qualified
acknowledgement. The mock fixture does. Physical takeover detection, hard
process-loss release and real macOS/Windows qualification remain outstanding.

`CodeRuntime.execute`, `ProcessSandbox` and sandbox selection fail closed:
primitive availability, claimed capability flags and injected objects cannot
enable model-code execution. No qualified launcher exists. The Linux prototype
inherited host memory/FDs and did not establish current-process PID/capability
isolation; its process dispatch has been removed rather than called contained.
No model source reaches a worker or a supplied sandbox callback.

## Deterministic checks

From this extension directory:

```console
python3 -m unittest discover -s tests -p 'test_*.py'
python3 -m py_compile main.py octet_computer_use/*.py
```

The suites use synthetic native fixtures only. They never open user applications,
request permissions, run a code worker or contact a provider. These checks
do not establish Windows/macOS installed-package or native-host qualification.

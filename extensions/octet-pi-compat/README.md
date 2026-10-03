# Optional Pi factory compatibility

One optional Node process loads an **explicit reviewed list** of unchanged Pi
extension factories. Rust still owns octet's agent, sessions, approvals, terminal,
keyboard focus and shutdown. This package never imports or starts the Pi
coding-agent runtime. It is not an OS sandbox or universal Pi compatibility claim.

## Local setup and configuration

Requires Node 22.19+ and npm. Nothing executes during extension discovery, and
neither helper enables an extension or writes host configuration/trust:

```sh
node extensions/octet-pi-compat/setup.mjs
node extensions/octet-pi-compat/configure.mjs --reviewed \
  --output /absolute/local-extensions/octet-pi-compat \
  /absolute/reviewed/extension.ts /absolute/another/extension.ts
# Explicit activation is a separate host/user decision:
octet --extension-dir /absolute/local-extensions --enable-extension octet-pi-compat
```

`--reviewed` authorizes executing those factories once in a bounded registration
capture subprocess. Review their imports and normal OS effects first. Configure
writes only the requested directory's `extension.toml` and `bridge.json`, uses the
canonical Node executable and this package's absolute runner path, and refuses
existing files unless `--overwrite` is explicit. It does not copy factories,
assets, or their dependencies. Install a factory's own dependencies separately.
The generated manifest is local/unpackaged; publishing an installable bundle also
requires the host's exact `requires_octet` pin and independently reviewed packaging.

The runner accepts `node runner.mjs --config /absolute/bridge.json`, with:

```json
{"extensions":["/absolute/reviewed/extension.ts"]}
```

Generated config additionally retains entrypoint SHA-256 and exact static
registration metadata. A changed entrypoint/catalog fails startup and requires
explicit reconfiguration. These hashes are change detection, not a sandbox or
an integrity claim over an extension's entire import graph. The shipped config
is empty and does not discover/import any Pi user installation.

Dependencies are pinned to jiti 2.6.1, `typebox` 1.1.12, legacy
`@sinclair/typebox` 0.34.41 and selected MIT Pi TUI 0.85.0 modules. The two
TypeBox packages remain distinct; both `@earendil-works` and old `@mariozechner`
Pi imports alias to the same host facades. Setup uses `npm ci --ignore-scripts`, without global
installs, asset downloads, providers or model calls. There is no coding-agent npm
dependency. jiti's disk transpilation cache is disabled.

## Implemented surface

- Static `registerTool`, `registerCommand`, `registerShortcut`, `registerFlag`,
  synchronous `getFlag`, and ordered `on` callbacks. Names/schemas are captured
  before activation; runtime registrations/completion callbacks are refused.
- A shared in-process `pi.events.on/once/off/emit` bus preserving synchronous
  order, object/function identity and retained owner contexts across factories.
- Text tool results, `details` retained in `metadata.pi_details`, explicit error
  results, declared structured outputs, cancellation signals and text progress.
  Raw Pi image/audio results and progress `details` cannot be represented and
  are refused; they are never silently discarded.
- Owner-bound `cwd`, `model` (truthful `model_view` conversion, including exact
  rate units), session name/ID, and optional host-supplied context usage, entries,
  branch/model/auth-status snapshots. Missing snapshots throw explicit
  `unsupported_feature`, not invented empty session data or credentials.
- Host notifications, active-parent confirmation/input, real remote selection
  and editor dialogs; useful composer/session-entry/user-message/active-tool
  operations only when negotiated. Setters update local mirrors synchronously,
  await host acknowledgement at live boundaries, roll back refused mirror writes,
  and report background refusals. Assistant/system provider turns and custom
  transcript display are explicitly unsupported.
- `ctx.ui.custom`, footer/header/widget/editor factories, safe component focus,
  input listeners and overlay composition. JS components, callbacks and timers
  stay live in this process. Only printable text and bounded safe SGR snapshots
  cross the wire; Rust paints cached lines and never calls JS synchronously.
- `setStatus` is local footer-provider metadata, not an assertion that every
  frontend displays ambient chrome. The compatibility palette is local, not a
  claim to reproduce octet's host theme; unknown roles are explicit errors.
- Selected TUI components/utilities, including `CustomEditor` based only on the
  Pi TUI `Editor`. `new TUI`, `ProcessTerminal`, terminal image escapes,
  `createAgentSession`, provider/auth storage and coding-tool runtime imports have
  no SDK fallback and are explicitly unsupported.

Hook registration maps `session_start`/`session_end`/`session_shutdown` to paired
API 0.4 lifecycle hooks, `tool_call`/`tool_result` to tool hooks, and
`input`/`before_agent_start`/`after_response` to available prompt/response hooks.
Negotiated turn/tool/message/compaction/model/dialog observations are dispatched
in order. Events expose only facts actually supplied by octet; reading absent Pi
fields (for example an `agent_end.messages` array) or returning an unapplied
transformation/veto fails explicitly. Command argument arrays join with spaces;
the wire cannot reconstruct original shell quoting.

**pi-clm 1.0.0 is not qualified.** The unchanged pinned entrypoint still refuses
unsupported `resources_discover` registration. Context-helper exports, effective
context/payload transformations, authoritative synchronous `appendEntry`, and
real compaction interception are not implemented. The selected 0.85.0 TUI/schema
profile is not a claim that all Pi 0.85.0 runtime imports or CLM's seven behavioral
gates work. `appendEntry` remains asynchronous; flushing a Promise after a hook
cannot satisfy CLM's persist-before-activate contract.

## Remote frontend integration

See `integration-contract.json` and octet's `docs/extensions/remote-ui.md`.
Negotiate `remote_ui` only on API 0.4 when offered; otherwise `ctx.hasUI` is false.
Complete host-issued owner triples fence every retained surface/context.
`context/updated` is `{resource_owner,host}`; unowned/foreign/stale updates cannot
change a retained footer. Existing `ui/editor-state {text,revision,focused}` is
negotiated with `editor_handoff` and mirrors the active owner's real composer.
Command-time `composer/get` also refreshes it from the actual host, never a fake
empty string. `model_view` snake-case fields map to Pi camelCase fields.

After a custom fullscreen surface's first frame, `command/execute` settles, but
the unchanged JS handler keeps awaiting `custom()` until the component's actual
`done(result)` or host dismissal. That continuation retains its numeric parent
and complete owner for background owner-safe operations. Ordinary footer/header/
widget setters await all openings before their request settles. Close, resize,
owner replacement, cancellation, EOF and shutdown dispose components and revoke
their timers; Rust remains the restoration authority.

Presses reconstruct legacy raw keys; modifiers/repeats/releases reconstruct Kitty
sequences (including original Doom's `wantsKeyRelease`). Ctrl+G and Ctrl+D remain
host-reserved. `ui/mouse` reconstructs SGR mouse input. The original drawing
extension's exact known `1000/1002/1006` enable/disable writes are interpreted
**only as capture intent** before `ui/open {mouse_capture:true}`; no raw escape is
written to a terminal. Other direct terminal-control writes fail explicitly.

Stdout is exclusively a captured serialized RPC writer. Console and direct
plain `process.stdout.write` calls are bounded stderr diagnostics. Transport has
1 MiB input/output frames, 128 writer slots/4 MiB queued bytes, latest-wins UI
snapshots, 128 outstanding reverse requests, 65,536 unique reverse IDs, 30-second
reverse deadlines, at most 8 host requests and 16 surfaces. Cancellation and
reverse replies stay serviceable while ordered hooks await; cooperative JS cannot
preempt CPU-bound code. Shutdown acknowledgement/drain is bounded to 1.5 seconds;
EOF/crash exits and the host owns final process-tree cleanup. Trusted code can
still access OS facilities directly: this isolation is protocol discipline,
not security confinement.

## Verification

```sh
npm test --prefix extensions/octet-pi-compat
```

Deterministic synthetic-host subprocess tests cover registration, isolated output,
owner-retained frames, cancellation/EOF/malformed transport, backpressure, shared
identity, ordered hooks, local mirrors, real choices and safe input/frame grammar.
Optional acceptance tests load original files unchanged via `PI_DOOM_PATH` /
`PI_DOOM_WAD`, `PI_FOOTER_PATH`, `PI_DRAW_PATH`, and `PI_RAINBOW_PATH`. They skip
when sources/assets are unavailable, never download or copy them, and exercise
real Doom WASM animation/resume, powerline footer snapshots, drawing mouse export,
and an animated CustomEditor. Synthetic-host acceptance does not establish actual
terminal painting. The separate Rust/frontend PTY suite has passed unchanged Doom,
drawing, the powerline footer, and terminal restoration in actual octet. Native
custom-editor draft restoration previously failed qualification, and startup comparison with
Pi remains unrun. The strict editor probe below now passes unchanged rainbow
input, post-rescue native draft/editability, resize and terminal restoration.
It also passed against the baseline adapter, so it does not establish a fix for
the historical native failure. A separate delayed-host-echo regression reproduces
and fixes loss of newer local input; writes/submissions are bounded and ordered.
No Doom aspect-ratio or original-Pi performance parity is claimed.

```sh
python3 extensions/octet-pi-compat/test/native-editor.py /absolute/octet \
  --editor /absolute/reviewed/rainbow-editor.ts
```

See [qualification and reproduction](../../docs/pi-compatibility.md#qualification-and-reproduction).

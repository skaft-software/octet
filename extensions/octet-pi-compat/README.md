# Optional Pi factory compatibility

One optional Node process loads an **explicit reviewed list** of unchanged Pi
extension factories. Rust still owns octet's agent, sessions, approvals, terminal,
keyboard focus and shutdown. This package never imports or starts the Pi
coding-agent runtime. It is not an OS sandbox or universal Pi compatibility claim.
Full stable Pi 1.0 compatibility remains the target; this source preview does not
qualify that target, and new generic resource APIs do not substitute for Pi parity.

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
dependency. jiti's disk transpilation cache is disabled. Pure context helpers
and command-argument parsing are adapted from Pi 1.0 commit `581e7ba78141a4d8b61cc9d11b8b22ae7e59195e`
under its [MIT license](LICENSE.pi); no Pi session store or agent runtime is imported.

## Implemented surface

- Pure `calculateContextTokens`, `estimateTokens`, and `buildSessionContext`
  exports on both coding-agent aliases. They operate on caller-supplied Pi data,
  including compaction-aware branches and context edits; they neither transform
  live native provider requests nor read/write the canonical session.
- Static `registerTool`, `registerCommand`, `registerShortcut`, `registerFlag`,
  synchronous `getFlag`, and ordered `on` callbacks. Names/schemas are captured
  before activation; runtime registrations are refused.
- Real sync/async `getArgumentCompletions` callbacks through negotiated native
  `autocomplete`. Registration follows initialization and requires admission.
  Callbacks receive Pi's complete raw argument prefix, with whitespace/quotes and
  Unicode preserved; native UTF-8 cursor offsets are validated, not treated as JS
  string indices. Pi attachment-token precedence is preserved. Results are bounded
  to 32 items / 1024 UTF-8 bytes per prefix/value/label/description, without controls.
  Unclaimed commands and null/empty callbacks return no items; exceptions, overflow,
  unsupported fields and unrepresentable quote/cursor edits fail explicitly.
- Tool `promptSnippet` / `promptGuidelines` map verbatim to tool-only catalog
  `prompt_snippet` / `prompt_guidelines` when `tool_prompt_metadata_v1` is negotiated.
  Declaring either field, even empty, requires the feature. Limits: 1024 UTF-8 bytes
  per string, at most 16 guidelines, C0/C1 forbidden except newline/tab. Pi projection
  trimming and aggregate catalog-budget enforcement remain native responsibilities.
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
- Custom editors require a host-issued mount ID and input revisions. Ordered
  draft checkpoints carry mount/input/checkpoint identity and require matching
  commit replies. A completed input event checkpoints final text even for cursor,
  consumed or release events; intermediate onChange calls cannot acknowledge it.
  Frames capture rendered lines and the exact checkpoint barrier together, then
  wait for that ACK before publishing. Clear/replacement drains the ordered tail;
  observed rescue/shutdown stays immediate. Old hosts without `editor_mount_id`
  explicitly refuse custom editors. During mount/seed initialization, at most 128
  normalized, fenced host events await their first component delivery. The actual
  open reply establishes the mount identity; seeding finishes before ordered input
  dispatch, and the first component frame awaits its captured checkpoint ACK.
  Rescue discards undispatched events immediately, never replaying them into the
  native editor. Current native qualification is pending below.
- Only fenced `composer/set` editor checkpoints from the mounted editor context
  have independent local lifetime while the opening request is live. Other reverse
  calls retain their live-parent attachment. The real parent ID, full owner and
  AbortSignal stay intact. Genuine request/child cancellation still aborts work,
  including cancellation after the opening reply. Nonretained operations require
  the actual live request; numeric ID reuse cannot revive an old captured context.
  Native admission needs the corresponding narrow checkpoint-lifetime rule;
  adapter-only tests cannot qualify it.
- Same-owner `setEditorText` and `pasteToEditor`, including cross-factory callers,
  mutate the active custom component and await its fenced checkpoint. Paste uses
  the component's real cursor API, never an invented append. Without a custom
  editor, ordinary native composer/set/insert behavior is unchanged.

Hook registration maps `session_start`/`session_end`/`session_shutdown` to paired
API 0.4 lifecycle hooks, `tool_call`/`tool_result` to tool hooks, and
`input`/`before_agent_start`/`after_response` to available prompt/response hooks.
Negotiated turn/tool/message/compaction/model/dialog observations are dispatched
in order. Events expose only facts actually supplied by octet; reading absent Pi
fields (for example an `agent_end.messages` array) or returning an unapplied
transformation/veto fails explicitly. Command argument arrays join with spaces;
the wire cannot reconstruct original shell quoting.

**pi-clm 1.0.0 is not qualified.** The unchanged pinned entrypoint still refuses
unsupported `resources_discover` registration. The three pure context-helper
exports work, but that is only part of the still-blocked factory/registration gate.
There is no live binding for dynamic resource-path discovery, provider-payload
replacement, session-tree observations, or cancellable compaction. Tool metadata
has a negotiated adapter mapping, not yet native prompt-projection qualification.
Effective context/payload transformations and authoritative synchronous
`appendEntry` are not implemented. The selected 0.85.0 TUI/schema
profile is not a claim that all Pi 0.85.0 runtime imports or CLM's seven behavioral
gates work. `appendEntry` remains asynchronous; flushing a Promise after a hook
cannot satisfy CLM's persist-before-activate contract.

## Command completion boundaries

A completion request is `{text,cursor,revision}` with a UTF-8 byte cursor and a safe
nonnegative revision. One process-owned chain is registered after `initialize`;
no numeric parent or session owner is invented. Native text/cursor/revision/focus
and provider-owner fences must reject stale results before display and acceptance.
Requests may finish out of order; the adapter preserves each snapshot/request ID,
not an invented cross-session revision counter. Cancellation terminates the wire
request promptly; a late callback result cannot publish a second reply. Arbitrary
trusted callback code is cooperative and cannot be forcibly preempted.

The current suffix-only wire cannot express consuming a closing quote *after* the
cursor or leaving the cursor *inside* a quoted directory choice. Those cases are
explicit `unsupported_feature` errors, not approximated edits. Tab-containing raw
prefixes still reach callbacks, but positive results cannot pass the native plain
text grammar. There is no Pi filesystem completion implementation in this bridge.
Native chaining/path fallback on an empty or unclaimed result requires its own
snapshot-fenced implementation and qualification; process tests alone do not prove
ordinary path Tab still works. Native trigger/selection behavior is not a claim to
reproduce Pi's automatic-versus-forced-file completion UI.

## Remote frontend integration

See `integration-contract.json` and octet's `docs/extensions/remote-ui.md`.
Negotiate `remote_ui` only on API 0.4 when offered; otherwise `ctx.hasUI` is false.
Complete host-issued owner triples fence every retained surface/context.
`context/updated` is `{resource_owner,host}`; unowned/foreign/stale updates cannot
change a retained footer. Existing `ui/editor-state {text,revision,focused}` is
negotiated with `editor_handoff` and mirrors the active owner's native composer
when no custom editor is mounted. It has no mount/input identity and cannot ACK
or overwrite a custom draft, even after its write queue drains. Command-time
`composer/get` refreshes only the native mirror, never a fake empty string. `model_view` snake-case fields map to Pi camelCase fields.

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
Completion tests execute actual factory callbacks, admission/cancellation, Unicode
byte ranges, out-of-order replies, quote/cursor refusals and strict result bounds.
Tool-metadata tests execute real tools and validate negotiation, empty declarations,
UTF-8/count/control limits and unchanged manifest shape. These are not native
completion-UI or effective-prompt qualification.
Optional acceptance tests load original files unchanged via `PI_DOOM_PATH` /
`PI_DOOM_WAD`, `PI_FOOTER_PATH`, `PI_DRAW_PATH`, and `PI_RAINBOW_PATH`. They skip
when sources/assets are unavailable, never download or copy them, and exercise
real Doom WASM animation/resume, powerline footer snapshots, drawing mouse export,
and an animated CustomEditor. Synthetic-host acceptance does not establish actual
terminal painting. The separate Rust/frontend PTY suite has passed unchanged Doom,
drawing, the powerline footer, and terminal restoration in actual octet. Native
custom-editor draft restoration remains **unqualified**. The strict paced rainbow
probe passes input, post-rescue native draft/editability (including a later edit),
resize and terminal restoration against the previously qualified macOS host
binary. It includes a grace period and also passed against the baseline adapter;
it does not prove in-flight handoff correctness or qualify a newly built host.

The historical stricter `--burst` probe reproduced loss of the complete draft
after Ctrl+G without that grace period: the old wire allowed unfenced serial
writes after rescue. That red evidence remains intact. The new adapter implements
the proposed native checkpoint seam and synthetic-host tests cover metadata,
matching ACKs, captured-frame barriers, no-op input, late echoes, immediate
observed retirement, refusal and setter routing. These do **not** qualify the
native host implementation: its DTO integration/build and current-source PTY
probes must pass before claiming immediate-rescue restoration. The new adapter
intentionally refuses that historical host for custom editors. Do not rely on
arbitrary draft preservation or assume unacknowledged input was recovered.
Startup/Pi runtime performance parity and Doom aspect ratio are not established
by these editor probes.

```sh
python3 extensions/octet-pi-compat/test/native-editor.py /absolute/octet \
  --editor /absolute/reviewed/rainbow-editor.ts
# Required outstanding gate: rescue immediately after a complete burst-input frame.
python3 extensions/octet-pi-compat/test/native-editor.py /absolute/octet \
  --editor /absolute/reviewed/rainbow-editor.ts --burst
# Optional offline comparison against source-extracted pure Pi 1.0 functions:
PI_REFERENCE_REPO=/absolute/reviewed/pi-checkout \
  node --test extensions/octet-pi-compat/test/context.test.mjs
# Source-hash-verified pinned commands.ts and Pi1.0 completion parser/apply oracle:
PI_COMMANDS_PATH=/absolute/reviewed/pi-1.0-commands.ts \
PI_REFERENCE_REPO=/absolute/reviewed/pi-checkout \
  node --test extensions/octet-pi-compat/test/completions.test.mjs
# Explicit non-support regression, NOT a CLM parity pass:
PI_CLM_PATH=/absolute/reviewed/pi-clm-b84a9d7c/index.ts \
  node --test extensions/octet-pi-compat/test/clm.test.mjs
```

See [qualification and reproduction](../../docs/pi-compatibility.md#qualification-and-reproduction).

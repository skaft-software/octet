# Optional Pi extension compatibility

Octet can load explicitly reviewed Pi extension factories through the optional
`extensions/octet-pi-compat` Node package. **Rust remains the agent and terminal
host.** The bridge does not run, embed, import, or install Pi's coding-agent
runtime. Ordinary octet use and native executable extensions do not need Node.

**Source preview:** unchanged Doom, drawing, and powerline-footer factories have
passed actual octet binary/PTY acceptance as well as synthetic-host tests. Native
custom-editor draft restoration still fails qualification; full Pi API parity
and a startup-speed comparison are not established.

The contract is **Pi 1.0.2's public extension API**, replicated under Octet's
subprocess JSON-RPC protocol. The [27-row ledger](pi-extension-api.md) is the
bounded scope and distinguishes implementation from real-host acceptance.
Arbitrary third-party extensions, private Pi internals, the Pi CLI and child SDK
are not targets. The [package README](../extensions/octet-pi-compat/README.md)
describes the adapter; historical package acceptance is not public API parity.

## Explicit local setup

Use an absolute path to a reviewed original extension entrypoint. Review its
imports as well: loading a factory executes ordinary unsandboxed JavaScript.
Do not point the adapter at an entire unknown npm installation.

```bash
cd /absolute/path/to/octet/extensions/octet-pi-compat
npm ci --ignore-scripts --no-audit --no-fund

node configure.mjs --reviewed \
  --output /absolute/workspace/.octet/extensions/octet-pi-compat \
  /absolute/path/to/reviewed-extension.ts
```

The helper captures registrations and writes a manifest plus a bridge
configuration containing entrypoint hashes. It does **not** enable or trust the
extension, change HOME, or download extension packages. It refuses existing
configuration without `--overwrite`. Registrations must still match when the
host starts it; changed entrypoints require fresh review and configuration.
Transitive imports are not sandboxed or made immutable by an entrypoint hash.

In that workspace, inspect `octet extensions list`, then explicitly enable and
trust the generated extension with the normal [extension
commands](extensions.md). Alternatively use octet's interactive
extension manager. Keep the Node package at its configured absolute path;
the generated manifest references it. Node dependencies are extension-local.

## UI execution model

Pi components render inside the adapter, using selected pinned Pi TUI utilities
and a headless component/editor driver. A Pi `ui.custom()` mount settles the
Rust command after its first frame while preserving the JS handler's pending
promise until completion/dismissal. Rust composes cached styled frames via
[`remote_ui`](extensions/remote-ui.md), routes focused keys and host-captured
mouse gestures, and owns terminal resize/restoration. There is no second stdin
reader, terminal renderer, or synchronous render RPC.

Header/footer/widgets are distinct generic host placements. The original footer
receives secret-free host model/session/context snapshots instead of fabricated
usage or OAuth data. Unavailable capabilities fail explicitly. Custom editor
state uses the host composer handoff rather than a separate agent input loop.
`Ctrl+G` rescues a focused component; `Ctrl+D` remains host-owned shutdown.

Raw extension terminal writes are not allowed into frames. The adapter recognizes
only allowlisted mouse-mode escape writes as capture intent, which the host can
refuse. Do not use unrelated Pi terminal patches, private session internals, or
session-file writers as compatibility assumptions.

## Installed-Pi fallback (experimental, opt-in)

If a reviewed factory imports a Pi export that Octet's shims lack, or one of Pi's
built-in tool factories (`createBashTool`, `createReadToolDefinition`, …), the
adapter refuses to start it and reports `pi_compat_fallback_eligible` with the
names it found. You can then load that bridge against the Pi packages from your
managed Pi install (`~/.pi/agent`, Pi 1.0.x only) with `--pi-runtime installed`
or `"pi_runtime": "installed"` in `bridge.json`. No Octet prompt offers this
yet; a permission prompt is planned.

This runs unpinned code from your installed `@earendil-works/pi-coding-agent`,
`pi-ai` and `pi-tui` inside the extension process:

- Pi's built-in tool factories run commands and file reads/writes in that
  process, outside Octet's per-command tool policy, other extensions'
  `tool_call` hooks, output spill and process-group supervision.
- Importing Pi installs process-wide signal handlers, an HTTP dispatcher and
  `fs` patches in that process.
- Octet still owns the `pi` API object, sessions, model calls, settings and the
  terminal. Pi functions that would start Pi sessions or agents, call models,
  read credentials, write Pi config or trust, start MCP servers, or own the
  terminal or clipboard are refused, as are names Octet has not classified.
- A missing Pi install, or one outside 1.0.x, fails loudly; nothing falls back
  silently.

## Qualification and reproduction

The automated adapter suite uses a synthetic host. A run with all four original
entrypoints explicitly supplied passed **22 tests, zero skips**, including Doom
WASM animation/resume, drawing mouse export, owner-retained powerline snapshots,
and rainbow-editor animation. Synthetic-host results alone do **not** establish
terminal painting. A separate actual octet binary/PTY run passed unchanged Doom,
drawing, the powerline footer, and terminal restoration on macOS. That run
observed an 88.1 ms first frame, a 270.0 ms Doom open, 31 frames/sec while turning,
and a 26.5 ms close. These are one-run observations, not a Pi speed comparison.

Native rainbow-editor acceptance failed to restore the complete draft after
rescue. Do not rely on custom-editor draft preservation yet. SDK/child-agent
semantics, private patches, and the wider extension corpus remain unqualified.

```bash
node --test extensions/octet-pi-compat/test/adapter.test.mjs
# Original-factory tests require explicitly reviewed local paths:
PI_DOOM_PATH=/absolute/pi-doom/src/index.ts \
PI_DOOM_WAD=/absolute/pi-doom/doom1.wad \
PI_DRAW_PATH=/absolute/draw.ts \
PI_FOOTER_PATH=/absolute/powerline-footer/index.ts \
PI_RAINBOW_PATH=/absolute/rainbow-editor.ts \
node --test extensions/octet-pi-compat/test/*.test.mjs
python3 scripts/test-pi-compat-pty.py --help
# Actual native UI acceptance (review the original sources/assets first):
python3 -B scripts/test-pi-compat-pty.py /absolute/path/to/octet \
  --doom /absolute/pi-doom/src/index.ts \
  --draw /absolute/draw.ts \
  --footer /absolute/powerline-footer/index.ts \
  --capture /absolute/native-ui.pty
```

Without those environment variables, original-factory probes skip; the suite
never searches HOME or developer temporary directories for executable factories.

The PTY script creates an isolated HOME/workspace, installs only supplied reviewed
entrypoints, and uses an offline model (no inference/credentials). Available
probes cover original `pi-doom`, Ben Vinegar's `/draw`, the installed powerline
footer, and Pi's example rainbow editor. It tests rendering/input/resize/rescue
and terminal restoration, not full Pi API parity. Editor acceptance runs
separately. `--pi-cli /absolute/pi/dist/cli.js` adds repeated
spawn-to-first-synchronized-frame measurements
for comparison; this is a startup metric, **not** an inference or throughput
benchmark. Consult the recorded test output for which probes actually passed.

The originals and their assets are not bundled into octet. Install them
separately and preserve their upstream [licenses and
notices](../THIRD_PARTY_NOTICES.md). Doom declares GPL-2.0; its WAD has separate
terms.

# TypeScript local tool example

One author source file, [`extension.ts`](extension.ts), registers a useful local
`text_stats` tool with inferred arguments and a real input schema. It counts
Unicode characters, words, lines and UTF-8 bytes. The SDK owns the process wire,
progress, cancellation and shutdown; authors do not write JSON-RPC.

Requires Node **>=22.19.0** and the reviewed **0.8.2 source SDK**. No provider,
credentials, network, global install or registry package is needed. From the
repository root:

```console
npm run pack:sdk --prefix examples/extensions/typescript-hello
npm install --offline --ignore-scripts --no-package-lock --prefix examples/extensions/typescript-hello
npm run manifest --prefix examples/extensions/typescript-hello
```

The local package installs the reviewed SDK tarball produced by `pack:sdk`,
not a mutable checkout symlink. Retain that archive when packaging. Manifest generation reads
registrations without invoking handlers and writes the exact API **`0.4`**
manifest and a staging-safe `.octet-launcher.sh` which execs installed Node in
place; the host stages the script, never the dynamically linked interpreter.
The generated manifest is
ignored by Git because those paths are machine-local; `package.json` is the
portable recipe. The generator refuses to overwrite an existing manifest.
After moving the checkout or changing registrations, intentionally remove the
old generated `extension.toml` and `.octet-launcher.sh`, then rerun `npm run manifest`.

Review the source before generation: importing a module executes its top-level
code. This example only registers its tool and exports the Extension. There is
no author-written launcher, manifest protocol, loader or compiler setup.

With a source-built API `0.4` host, explicitly enable:

```console
octet --extension-dir ./examples/extensions --enable-extension typescript-hello
```

Check `/extensions status`. A call with `{text: "hello world\n🦀"}` returns:

```text
characters=13 words=3 lines=2 utf8_bytes=16
```

Optional `delayMs` (0..2000) makes cooperative cancellation easy to exercise.
Cancel an in-flight call with `delayMs: 2000`; the SDK settles it as cancelled,
without inventing rollback. Progress is emitted only if the host offers it.
Clean shutdown is handled by the runtime; no callback is needed for this
stateless tool.

Discovery never enables or runs code. The local manifest has no host-version
pin and declares no filesystem/process/network capability. Capability metadata
is **not an OS sandbox**. Full access trusts selected explicitly enabled
extensions without persisting a grant; safe/controlled policies require explicit
source-bound host authority. This is a local source recipe, not an installable
bundle or an npm/native publication claim.

Provider-free checks from the repository root:

```console
npm --prefix sdk/typescript run test:process
cargo run --offline --manifest-path sdk/typescript/host-smoke/Cargo.toml
```

The process test suite includes an offline packed-SDK TypeScript consumer with
initialize/tool/progress/cancel/shutdown. The Rust smoke uses real discovery,
explicit enablement and source-host process supervision. See the
[authoring API and bounds](../../../sdk/typescript/process/README.md) and
[host smoke](../../../sdk/typescript/host-smoke/README.md).

Launcher generation is currently Unix-only. Actual launch qualification is macOS;
Windows native launch remains unqualified and generation explicitly refuses it.

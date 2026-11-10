# Pi 1.0.2 helper sources

`../EXPORTS.pi-1.0.2.json` inventories the public runtime names from the reviewed
managed Pi 1.0.2 distribution: 127 `pi-ai/compat`, 156 `pi-coding-agent`, 89
`pi-tui`, 8 `pi-agent-core`, and the `pi-ai/providers/all` subpath. Both Pi scopes
resolve the same shims. Type-only exports do not create runtime values.

Bindings are classified as:

- `local`: a local computation, static value, or caller-owned utility. Some read
  caller-selected files or environment variables; this is not a sandbox.
- `managed-local`: a reviewed individual Pi dist module, loaded lazily from a
  supported locally installed package or the configured managed Pi installation.
  These helpers require that installation and its dependencies; they do not load
  a second agent. The resolved helper must remain inside its package directory.
- `host`: an adapter facade or admitted native-provider delegate. It still needs
  its normal negotiated feature, request/owner and native admission.
- `refused`: a named import exists, but invoking it explicitly refuses. This
  includes second agent loops, independent extension runners, credential stores,
  package/trust mutation, raw terminal ownership and unadmitted inference.

`ai/*.mjs` contains Pi's text, transcript, event stream, model operations,
retry/overflow, diagnostics and caller-owned auth/lazy helpers. Tool-argument
validation is no longer adapted from 1.0.2: Pi 1.5 replaced TypeBox with
pi-schema, and `../pi-1.5.0/ai/validation.mjs` is the single port (the installed
release's own `dist/utils/validation.js` is preferred whenever one answers, see
`../manifests/`).
Lazy streams share the public shim's event-stream class identity rather than
creating a second managed-Pi class graph. `coding-pure.mjs` contains
skill prompt formatting and language detection. Imports are adapted to local
modules and comments/source-map trailers removed. `models.json` is the generated
chat/image/classifier inventory, not the host's selected model, credentials or
available-model snapshot. `tool-definitions.json` contains function-free builtin
metadata generated from the reviewed Pi 1.0.2 tool constructors; execution stays
in the native broker, never those Pi implementations. `UPSTREAM.json` records
source hashes. These adapted
sources are covered by [Pi's MIT license](../../LICENSE.pi).

`../ai-providers.mjs` uses that static inventory and guarded native streaming,
not Pi's HTTP/provider runtime. Provider authentication remains native-owned.
Environment auth helpers consume the extension's own environment or explicitly
supplied callback/credential values; they never resolve the host's credential
store. Lazy adapters only invoke the caller-supplied setup implementation.

Regenerate builtin metadata only from an explicitly reviewed reference:

```sh
node scripts/build-builtin-metadata.mjs /absolute/pi/node_modules/@earendil-works \
  > shims/pi-1.0.2/tool-definitions.json
```

`../../test/shim-inventory.test.mjs` checks all inventoried names and refusal
boundaries, and compares builtin metadata against those original constructors. Set `PI_SHIM_REFERENCE_DIR` to the reviewed
`node_modules/@earendil-works` directory for direct pure-helper/model comparisons.
An export inventory, a passing import, or registration capture is not complete
API, original-package, native-host or terminal qualification. In particular,
the hosted `ExtensionRunner` is the live catalog/dispatch instance, not a dummy
prototype. Captured tools and supported builtin factories require live native
composition/preflight admission. Raw installed-Pi tool execution is never granted;
grep/find/ls and Fabric's custom preview-write path remain unqualified.

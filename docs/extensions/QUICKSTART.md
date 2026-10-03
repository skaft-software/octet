# Build, install and maintain a local extension

Start with a small domain tool, not a protocol loop. The SDK handles stdio,
initialization, request dispatch, cancellation and shutdown. **One author file
is not one deployment file**: retain the SDK, launcher, manifest and runtime
requirements too. These recipes use reviewed source and existing local runtimes;
none requires a paid provider, registry publication or global SDK installation.

## Status and contract boundaries

This guide targets feature-negotiated **extension API `0.4`** and the 0.8.2
candidate source host. Extension version, SDK distribution version, host version
and API version are independent. Read [the host guide](../extensions.md) for
current admission rules and [the wire reference](PROTOCOL-REFERENCE.md) when
implementing a service outside a helper.

The Python, JS/TS and narrow native helper/example sources are integrated in
this candidate. Experimental Ruby/Bash helpers are withheld, not supported SDKs. They are
local source recipes, not published SDK packages or catalog assets. The Python
SDK README still describes distribution 0.8.0 and has an obsolete safe-mode
statement: use the current host guide for admission rules, and inspect SDK
metadata rather than inferring its distribution version from the host version.

API `0.4` uses compact JSON-RPC JSON lines and a negotiated `protocol` feature
list. The retained **canonical API `0.3`** uses a distinct strict canonical wire
and `contract` negotiation. Generated Python `api_v03` / TypeScript canonical
bindings are contracts, not complete process helpers. Do not retag their
manifests as `0.4`. Neither wire nor these helpers establishes full **Pi API/SDK
parity**: Pi imports, callbacks, lifecycle, UI and synchronous behavior require
separate qualification. The native helper is tool-only, not an in-host C plugin
or [native-host embedding protocol 1](../sdk.md).

## Choose a language

| Author | Local requirements | Scope and observed limits |
| --- | --- | --- |
| Python | SDK metadata declares Python >=3.9; current generator requires Python >=3.11 (`tomllib`); reviewed `sdk/python` source | API 0.4 runtime; single-file generator covers static tools, not inferred schemas, commands or hooks. Local generated archive and relocated real-host smoke passed in the author lane; parent separately reports five integration generator tests passed. |
| JavaScript / TypeScript | Node >=22.19.0; local SDK tarball; `type: module` | ESM `.mjs` or erasable `.ts`, no loader/compiler required to run. Tools, fixed commands, optional progress; not hooks/UI/reverse services. Launch tested on macOS Node 22.20.0/22.23.2, not exact 22.19.0. Generator refuses Windows; Linux native launch unqualified. |
| Rust | Rust/Cargo; declared MSRV 1.88; pinned crates cached for offline builds | Static tools, typed generated input schemas, text results; observed Rust 1.97.1 on macOS arm64, not MSRV qualification. |
| C / C++ | Same Rust build plus C11 / C++17 compiler/linker | C ABI 1 / C++ wrapper over Rust runtime; flat string/integer/boolean input fields. macOS arm64 static-link/process evidence only; Linux/Windows/cross-target/dynamic deployment unqualified. |

Native deployment needs a matching target OS/architecture/libc and system-library
baseline. A Rust toolchain is needed to build the runtime even for C/C++.
Uncached dependencies need your normal approved acquisition workflow; `--offline`
is not a promise to build on a fresh machine. No precompiled SDK or published
npm/PyPI/crate availability is claimed.

## Write only domain code

Keep stdout for protocol; diagnostics go to stderr. Declare a real bounded
input schema where arguments exist. Check cancellation between effects;
cancellation is not rollback or permission to replay ambiguous work.

### Python: a local one-file tool

```python
from octet_extension import Extension

ext = Extension(api_version="0.4", max_concurrent_requests=1)

@ext.tool(name="hello", description="Return a local greeting")
def hello(args):
    ext.cancellation.raise_if_cancelled()
    return "Hello from octet."

ext.run()
```

For the existing cancellable `wait` source, generate a self-contained local
package without importing author code or globally installing the SDK:

```sh
# From the integrated repository root; output must not already exist.
python3 scripts/extension-author.py \
  examples/extensions/python-single-file/extension.py \
  /tmp/my-extensions/wait-extension --name wait-extension --tool wait
```

Inspect the generated `extension.toml`, `author.py`, launcher, vendored SDK,
license and provenance. The generator accepts literal static tool declarations;
extra modules/assets must be included deliberately. See the
[Python SDK](../../sdk/python/README.md) and
[single-file recipe](../../examples/extensions/python-single-file/README.md).
The generator pins the actual workspace host version (`=0.8.2` here) and emits
relative executable `run-extension`. That launcher uses `OCTET_EXTENSION_DIR`
when staged by the host, selects the vendored SDK and suppresses bytecode writes;
no global SDK installation or original checkout path is needed.

Package this reviewed generated directory using the existing release packager:

```sh
bash scripts/package-octet-extension-release.sh \
  wait-extension /tmp/bundles v0.8.2 /tmp/my-extensions/wait-extension
# Check the exact produced archive and digest before installation.
# Extract to a separate reviewed location, then run the provider-free smoke:
env -u PYTHONPATH CARGO_TARGET_DIR=/tmp/python-single-file-host-target \
  CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 cargo run --offline --locked --profile ci-test \
  --manifest-path examples/extensions/python-single-file/host-smoke/Cargo.toml \
  -- /path/to/unpacked/wait-extension
```

Use the actual workspace version, not a fabricated pin. The final Python author
report qualifies local archive creation (two byte-identical builds) and a
relocated/extracted real-host discovery, explicit test-only enablement,
initialization, tool call, cancellation, subsequent responsiveness and shutdown
with `PYTHONPATH` unset. The parent separately reports five generator tests
passed after integration; that is not an independent rerun of the entire author
archive/host-smoke qualification. No published/catalog qualification or live
CLI/model/provider acceptance is inferred.

### JavaScript / TypeScript: schema plus handler

```ts
import { Extension } from '@skaft-software/octet-extension-sdk';
const extension = new Extension();
extension.tool({
  name: 'greet', description: 'Return a local greeting',
  parameters: {
    type: 'object', properties: {name: {type: 'string', maxLength: 256}},
    required: ['name'], additionalProperties: false,
  },
}, ({name}, context) => {
  context.throwIfCancelled();
  return `Hello, ${name}!`;
});
export default extension;
```

The same source without types can be `.mjs`. Node's type stripping does not
support enums, parameter properties, JSX or tsconfig path aliases. Export the
Extension from CLI-loaded modules; do not call `run()` there.

Build the existing useful `text_stats` example locally:

```sh
npm run pack:sdk --prefix examples/extensions/typescript-hello
npm install --offline --ignore-scripts --no-package-lock \
  --prefix examples/extensions/typescript-hello
npm run manifest --prefix examples/extensions/typescript-hello
```

Review source **before manifest generation**: module registration executes
its top-level code, though handlers are not invoked. This installs a copied SDK
tarball, not a mutable checkout `file:` symlink. Keep the archive/hash. The
machine-local manifest points at `.octet-launcher.sh`, which execs the original
installed Node and installed SDK in place; don't relocate Node and its dylibs.
After a move, contribution change or Node-path change, deliberately regenerate
both generated files (the generator refuses overwrites). See
[process helper](../../sdk/typescript/process/README.md) and
[text statistics recipe](../../examples/extensions/typescript-hello/README.md).

### Ruby and Bash: experimental helpers withheld

The language-neutral executable protocol can support Ruby or Bash, but the
supplied experimental helpers/examples are excluded from this candidate pending
repairs and real native-host tests. Do not use their prototype install/launch
instructions as a supported SDK path. Independent review reproduced Bash's
missing SDK under sanitized host launch, detached tool groups escaping host
cleanup and descendants surviving shell exit; Ruby also failed settlement,
concurrency admission, bounded shutdown and boundary validation. Passing
happy-path subprocess tests does not waive these issues. Choose Python/JS/TS
or the narrow native helper above; implementing another runtime requires the
actual API 0.4 contract and adversarial lifecycle/host tests.

### Rust / C / C++: build an executable

Each [native hello source](../../examples/extensions/native-hello/README.md)
implements greeting and interruptible waiting without handwritten RPC. The
[Rust helper](../../sdk/rust/README.md) derives schemas; the
[C ABI](../../sdk/c/README.md) copies definitions/results while callback arguments
are borrowed; the [C++ wrapper](../../sdk/cpp/README.md) owns callable storage and
contains exceptions. Never retain callback handles or throw/longjmp across C.

```sh
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 \
  bash examples/extensions/native-hello/build.sh
python3 sdk/rust/tests/test_process.py
# Reusable one-source C/C++ templates:
make -C sdk/c OCTET_ROOT="$PWD"
make -C sdk/cpp OCTET_ROOT="$PWD"
```

Generated manifests live in `examples/extensions/native-hello/build/extensions/`
and pin `requires_octet = "=0.8.2"`. Choose one language to avoid duplicate
`hello` tools. Rebuild/regenerate after moving the checkout. Native helpers
support static tools and one text result/error only: no hooks, commands, UI,
flags, dynamic catalogs, media, reverse host services, secrets, approvals,
composition or sessions. Unsupported contributions/features fail explicitly.

## Discover, enable and grant authority separately

A discovery root contains direct children whose names match their manifests:
`my-extensions/hello-tools/extension.toml`. The child cwd is the workspace,
not the package; review entrypoints and paths accordingly.

```sh
# Explicit directory selection grants invocation-only source authority,
# but this separate enable flag is still required.
octet --extension-dir /tmp/my-extensions --enable-extension wait-extension
# Existing TS recipe:
octet --extension-dir ./examples/extensions --enable-extension typescript-hello
# One native language only:
octet --extension-dir ./examples/extensions/native-hello/build/extensions \
  --enable-extension native-hello-rust
```

These are manual host-start flows, not provider-free acceptance tests. Inspect
`/extensions status` before deciding to run a model. Installation/discovery
never executes code, enables, grants persistent trust or runs setup.

Global discovery precedes trusted project discovery, then explicit directories
in CLI order; later same-named sources win. Projects require workspace trust.
Full access implicitly authorizes selected enabled processes but never enables
them or persists a grant. Controlled/safe mode needs explicit source-bound host
authority; `--trust-extension NAME` grants that for one invocation and **does
not enable**. Persistent activation is `enabled_extensions`; persistent grants
are separate `trusted_extensions`. Bare grants apply only under
`~/.octet/extensions`; project/other grants use
`NAME@/absolute/path/extension.toml`. Review the selected path, not just its name.
`--no-process` / `--no-shell` gates can still refuse startup.

## Package and install a reviewed archive

Local generated directories are not automatically release-format archives.
Installed bundles need an exact `requires_octet` matching the running host,
a matching root/manifest name, supported API and portable safe archive entries.
The [bundle reference](legacy-authoring.md#installable-extension-bundles)
describes validation and the existing release packager. A local archive must
contain its complete reviewed runtime closure; there is **no install hook**
to run pip/npm, download servers or provision dependencies.

For a release-format source directory that meets those requirements, the
existing packager takes the source explicitly:

```sh
# Example placeholders: supply your reviewed source and exact host version.
bash scripts/package-octet-extension-release.sh \
  hello-tools /tmp/bundles v0.8.2 /absolute/path/to/hello-tools
# Use the actual produced archive path, not a guessed filename.
octet extension install --path /absolute/path/to/reviewed-bundle.tar.gz
octet extension list
# Only after review, separately opt into runtime activation:
octet --enable-extension hello-tools
```

An unpackaged manifest without the exact host pin must not be passed off as
installable. In particular, TS's generated launcher/absolute paths are local,
not a portable release bundle; Python's generated relative-launcher package has
local release-format archive evidence, not publication qualification.
The installer rejects links, special files, unsafe paths and mismatched
API/host identity, records SHA-256, and atomically publishes a validated staged
candidate. A hash detects byte changes; it does not establish authorship or
safety. Do not claim catalog installation until matching assets are published
and verified. These commands are guidance, not actions performed by this guide.

## Test, upgrade and roll back the whole closure

Before promotion, use provider-free process/host fixtures, not just imports or
header compilation. Check disabled discovery, explicit admission, exact
negotiation/catalog, one real tool call, malformed arguments, cooperative
cancellation, subsequent generation health and clean shutdown. Relevant checks:

```sh
python3 scripts/test_extension_author.py
PYTHONPATH=sdk/python python3 -m unittest discover -s sdk/python/tests -q
npm --prefix sdk/typescript run test:process
CARGO_BUILD_JOBS=1 cargo test --manifest-path sdk/rust/host-check/Cargo.toml \
  --offline --locked --lib
python3 scripts/generate-extension-api-v03.py --check
```

Run each with its required local runtimes, dependencies and build outputs ready. Native
host tests qualify the library boundary, not every frontend. Refreshed independent
macOS review closed both prior TS defects and passed the native process suites;
shared host changes still require integration rechecks. Preserve canonical tests.

For each version retain author source, manifest, SDK tarball/vendor tree and
licenses, lockfiles, launcher, target binary, runtime/toolchain identities and
archive digests. Preserve dependencies outside an archive too (Node/Python,
shared libraries, installed LSP binaries). A launcher
referencing a mutable SDK checkout is not a closed rollback artifact.

Disable before replacing files; inspect the candidate's closure and pin before
`octet extension update --path /absolute/path/to/candidate.tar.gz`. Updates
require the same managed ID, validate before swap and restore the old directory
on publication failure. That automatic failure rollback is **not** behavioral
rollback or reversal of external effects. Keep the prior reviewed compatible
archive and its dependencies: an intentional restore uses the local update
path with that prior archive, subject to current host/API validation. A host
version change may also require restoring the matching host; don't erase a pin
to force admission. Re-enable only after review and repeat the smoke checks.
`/extensions reload` is candidate-first for compatible running catalogs; changed
contributions may require a full `/reload` product rebuild.

`octet extension remove NAME` removes only a managed bundle, not configuration,
sessions or external state. Separately remove activation and explicit grants
when retiring it. Revoking a grant alone does not override full-access implicit
authority: turn the extension off too.

## Useful existing domain references and security limits

[git-tools](../../examples/extensions/git-tools/README.md) is a **legacy API
0.1** reference for bounded read-only `git status` (`GIT_OPTIONAL_LOCKS=0`, no
shell/commit) and checkpoint preview. Its metadata/renderer does not establish
frontend persistence/rendering. [lsp-client](../../examples/extensions/lsp-client/README.md)
is a **legacy** read-only navigation/diagnostics reference with bounded server
requests, document resync and typed unavailable results. It requires existing
`rust-analyzer`, `pyright-langserver` or `typescript-language-server`; it never
installs them. Its fake-server tests are not qualification of every real server.
Neither example is a current-API or full Pi SDK acceptance fixture. Check its
actual manifest/API/host pin before using it; do not retag its manifest or copy
obsolete safe-mode guidance.

Executable extensions have your OS authority. Capability metadata and protocol
bounds are **not a sandbox**; even a granted safe-mode extension runs outside
the tool-effect broker. Raw filesystem/network/process effects are not magically
brokered. Use separate OS isolation for untrusted work; bound subprocesses and
output, keep secrets out of logs/results, and never infer success from transport
loss or replay ambiguous unsafe effects. The host owns approval policy, sessions,
persistence, supervision and cleanup; helpers do not replace that authority.

Optional Codemode is separate and currently requires Python 3.11+ and Node 22.19+
for its launcher and pinned QuickJS/WASM runtime. These native tools do not imply a no-Node Codemode or
shared Pi runtime qualification.

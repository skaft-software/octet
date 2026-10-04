# One-source native hello tools (Rust / C / C++)

Local **octet 0.8.2 / feature-negotiated API 0.4** recipes. Three small author
sources, one transport/lifecycle implementation in [`sdk/rust`](../../../sdk/rust/README.md).
The [C ABI](../../../sdk/c/README.md) and [C++17 wrapper](../../../sdk/cpp/README.md)
use that runtime, not C plugins loaded into octet. This is **static-tool authoring**,
not full SDK feature parity or a parallel provider/agent runtime. Existing
canonical 0.3 examples/contracts remain unchanged.

Sources: [`rust/main.rs`](rust/main.rs), [`c/main.c`](c/main.c),
[`cpp/main.cpp`](cpp/main.cpp). Each implements only `hello`'s domain behavior:
required `name` (up to 256 Unicode scalars), optional `delay_ms` (0..5000), then
`Hello, <name>!`. The SDK hides envelopes, initialization, cancellation reads,
framing and shutdown. Delayed work uses cooperative interruptible waits; no
rollback or unsafe replay is implied.

## Build and exercise locally

Prerequisites: Rust/Cargo (declared MSRV 1.88), platform C11/C++17 compiler/linker,
Bash and Python 3; pinned ecosystem dependencies cached for the offline recipe.
No global install or configuration change is performed. A Rust toolchain is
required even for the C/C++ runtime build. Source SDK registry/publication and
precompiled archive availability are not claimed.

From the checkout root:

```sh
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 bash examples/extensions/native-hello/build.sh
python3 sdk/rust/tests/test_process.py
```

This creates ignored `build/hello-rust`, `hello-c`, `hello-cpp`, the process-test
probes and **runnable local manifests** under
`build/extensions/native-hello-{rust,c,cpp}/extension.toml`. Absolute native
entrypoint paths are generated, because the child cwd is the workspace, not its
package. Moving a built checkout requires rebuilding/regenerating those manifests.
Each declares API 0.4, its own version 0.1.0 and exact host pin `=0.8.2`; the pin
is local compatibility metadata, not evidence of release publication or a wire
translation. It is deliberately not changed to match a different host silently.

The checked-in root `extension.toml` is also runnable after the default build:
it selects the Rust executable via `build/hello-rust`, resolved beside this
manifest. For C/C++ choose the corresponding generated manifest, rather than
renaming that root source directory or silently editing its entrypoint.

Reusable build templates:

```sh
# Standalone single Rust-source project (own workspace + checked-in lockfile):
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 \
  cargo build --manifest-path examples/extensions/native-hello/rust/Cargo.toml --offline --locked
# C/C++ templates; override MAIN/OUTPUT for your own single source:
make -C sdk/c OCTET_ROOT="$(pwd)"
make -C sdk/cpp OCTET_ROOT="$(pwd)"
python3 sdk/rust/tests/check_templates.py
# From this example directory: make all / test / rust / sanitize
```

Static C/C++ recipes have macOS/Linux system-library branches; only local macOS
arm64 is qualified here. Windows (MSVC/GNU), Linux, cross-compilation, the declared
MSRV and dynamic-library deployment have not been qualified. Binaries need a
matching OS/architecture/libc baseline. There is no zero-toolchain source runner.

## Explicitly enable in a reviewed host

With a matching source-built host supporting API 0.4, enable **one** language to
avoid duplicate `hello` catalogs:

```sh
octet --extension-dir ./examples/extensions/native-hello/build/extensions \
  --enable-extension native-hello-rust
# Or native-hello-c / native-hello-cpp, not all three at once.
```

Discovery doesn't execute code. Full access or an intentional source grant
supplies launch trust only to a validated selected enabled extension. Explicit
CLI directories convey source authority, but don't automatically enable their
extensions. Default/controlled global discovery requires a source-bound grant;
frontend process policy may still refuse launch regardless of that grant.
Executable code has your OS authority; capabilities
are consent metadata, **not a sandbox**. Review source/builds and use OS isolation
for untrusted code. See [current extension trust/enablement](../../../docs/extensions.md).
No test here invokes a model or modifies user configuration/auth.

Inspect `/extensions status`; a model call `hello({"name":"world"})` should
produce `Hello, world!`. Ask for `delay_ms=5000` and cancel to exercise cooperative
cancellation; exit to exercise graceful shutdown. This describes a manual user
flow, not a claim that live-model acceptance was run.

## Actual evidence path, without models

```sh
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 \
  cargo test --manifest-path sdk/rust/Cargo.toml --offline --locked --lib
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 CARGO_TARGET_DIR="$PWD/sdk/rust/target" \
  CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo test --manifest-path sdk/rust/host-check/Cargo.toml --offline --locked --lib -- --nocapture
python3 scripts/generate-extension-api-v03.py --check
```

The test-only companion uses actual `ExtensionProcess` initialization/decoder,
model-tool input-schema validation, dropped-waiter cancellation/tombstones,
generation-health/reuse and shutdown, with all three compiled processes. It also
loads the checked-in source manifest through the real catalog/policy: disabled
and untrusted global-source descriptors are rejected, then an exact source grant
allows its relative Rust entrypoint and tool/shutdown. This is library-level host
qualification, not a test of every frontend policy. A
separate compatibility check runs the retained canonical 0.3 process with its
source bytes/API/version unchanged and only a **private temporary** `=0.8.2`
host pin; its checked-in `=0.8.0` release pin is not changed. Raw
process tests additionally exercise bad/oversized/exact-bound frames, bounded
initialization, input rejection, unsupported contributions, text/error results,
panic/C++ exception containment, null/length/UTF-8 checks and C definition/result
copy lifetimes. A successful header compile alone is not process qualification.

Available compiler sanitizer check (does not install a sanitizer):

```sh
SANITIZE=1 OCTET_NATIVE_BIN_DIR="$PWD/examples/extensions/native-hello/build-sanitized" \
  bash examples/extensions/native-hello/build.sh
OCTET_NATIVE_BIN_DIR="$PWD/examples/extensions/native-hello/build-sanitized" \
  python3 sdk/rust/tests/test_process.py
```

Only C/C++ author/probe code is ASan/UBSan-instrumented; the stable Rust static
library is not, so this isn't whole-runtime sanitizer coverage.

## Rust typed, native-resource and binary-data recipes

[`rust/typed.rs`](rust/typed.rs) declares each input/output once: bounded finite
samples become a typed summary, with optional units, cancellation, negotiated
progress and an empty-input diagnostic. No resource or bulk setup is needed.
[`rust/resources.rs`](rust/resources.rs) declares a counter's nominal type once,
then registers a constructor, ordinary-argument update and explicit release-last
lifecycle operation. Release returns typed retirement and independent cleanup status.
[`rust/blobs.rs`](rust/blobs.rs) writes and reads immutable data with callback-scoped
streams; no author-written schema, slot list, private locator or byte-bearing RPC.
These additions do not change the three basic hello examples or C/C++ ABI1.

```sh
CARGO_BUILD_JOBS=1 bash examples/extensions/native-hello/build.sh
python3 sdk/rust/tests/test_resource_process.py
python3 sdk/rust/tests/test_bulk_process.py
```

Checked-in manifests are [typed/extension.toml](typed/extension.toml),
[resources/extension.toml](resources/extension.toml) and
[blobs/extension.toml](blobs/extension.toml). The build stages each Cargo example
into its bundle's ignored `build/` directory, without parent traversal or global
installation. Use a reviewed
API 0.4 host with session-owned context. The blob example additionally requires
host-configured bulk storage and negotiated `local-file.v1` (Unix helper profile);
it explicitly refuses an unconfigured host rather than placing bytes in JSON.
The production-host companion includes actual SDK resource/bulk processes. See
[SDK resource and bulk contracts](../../../sdk/rust/README.md#native-resources-and-operations)
for provisional output admission, explicit cleanup/failure semantics, finite limits
and callback lifetime constraints. These library-level checks are not a claim of
live-model, frontend, cross-platform or complete Pi SDK acceptance.

After staging, the ordinary tool flows are:

- `summarize({"label":"voltage","samples":[1,3],"unit":"V"})` → typed
  `{label:"voltage",count:2,mean:2,unit:"V"}` plus `Sample summary ready`.
  Empty samples return a domain error with `samples.empty`, not fake output.
- `counter_create({"initial":7})` → retain its returned `counter`; pass that exact
  identity to `counter_add({"counter":...,"amount":5})` → `{value:12}`.
  `counter_release_last({})` retires the last-created counter; reuse is refused.
  Older counters remain host-owned until explicit host release/session teardown.
  Do not add a Resource argument to this release tool: admission would pin it.
- `blob_save({})` → retain the returned `data`; pass it to
  `blob_size({"data":...})`. Every read verifies bytes and closes/releases its lease.
  Blob result retention belongs to the host/session, not an extension-side destructor.

To author a deliberately quiet extension, call `extension.request_progress(false)`
before `run`. Reusable handlers can use `call.supports_progress()`; a declined
progress helper returns an explicit error rather than emitting a notification.
The ordinary default remains opt-in when offered by the host.

The companion `author_examples` tests execute these staged manifests, including
release/reuse refusal and repeated fresh blob reads. Test sources are not claims
of successful execution; the parent integration run supplies that evidence.

## Deliberate limits

One active domain handler; static tools only. These hello examples and C ABI 1
return one text result/error. Rust additionally supports generated typed
input/output contracts, structured results, diagnostics and negotiated status
progress, native resources and local-file bulk helpers; see [Rust typed authoring](../../../sdk/rust/README.md#typed-inputoutput-diagnostics-and-progress).
Media rendering is not exposed by this SDK. No hooks/UI/commands/flags, dynamic catalogs, general reverse host
requests beyond resource/bulk helpers, secrets/approvals, composition, sessions or subagents. Unsupported
required features and manifest contributions fail explicitly. C/C++ expose flat
string/integer/boolean inputs only; Rust adds typed nonrecursive generated schemas.
Do not retain callback handles, bypass stdout framing, free live callback data,
or claim brokered-effect/sandbox authority for raw process effects.

Shutdown/EOF stops admission and cancels/joins. If a callback remains active
past the 500 ms drain, the executable exits 70 rather than returning with borrowed
C/C++ state live. The host remains final supervisor for I/O backpressure and
uncooperative cancellation. See the SDK's [complete limits and missing capabilities](../../../sdk/rust/README.md).

# Native executable-extension SDK (source 0.8.2)

**One Rust runtime; Rust authoring, a small C ABI, and a C++17 wrapper.** This is
an initial **static-tool authoring SDK**, not full Python/TypeScript SDK feature
parity. Authors implement domain functions; the SDK handles API 0.4 negotiation,
JSON-RPC envelopes, UTF-8 JSONL, validation, cancellation and shutdown.

It builds an ordinary executable supervised by octet. It does **not** load C
plugins into the host, embed a parallel agent/provider runtime, or replace
[native-host protocol 1](../../docs/sdk.md). Rust remains host authority for
selection, enablement/trust, effects, model conversations, persistence and process
supervision. Capability declarations are consent metadata, not an OS sandbox.

This checkout is the source distribution. There is no crate/C/C++ registry or
publication claim, and no zero-toolchain Rust-source execution. A Rust toolchain
is required to build the runtime even for C/C++; deploy the resulting native
executable to a compatible platform. The SDK crate is deliberately `publish =
false` and a **separate Cargo workspace with its own checked-in lockfile**. It
never changes or builds the full root workspace.

## Build and run the three one-source recipes

From the repository root, with Rust/Cargo (declared MSRV 1.88), C11, C++17, a
platform linker and Python 3 for local test/manifest tooling:

```sh
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 bash examples/extensions/native-hello/build.sh
python3 sdk/rust/tests/test_process.py
```

The offline locked build needs the checked-in dependencies in the local Cargo
cache. An uncached fresh environment must acquire the pinned ecosystem crates by
its normal approved dependency workflow; this recipe does not silently fetch or
install tools. Direct runtime dependencies are Serde, serde_json and Schemars,
all standard cached libraries used in this checkout; no host crates are linked
into author executables. Static C/C++ linking uses platform Rust-std system
libraries. A `cdylib` is also built, but dynamic deployment/loader paths are not
qualified by these recipes.

See [native-hello](../../examples/extensions/native-hello/README.md) for handwritten
Rust/C/C++ functions, local manifests, Make and standalone Cargo templates,
explicit enablement, and expected output. By default its tools are `hello` with
required `name` and optional bounded `delay_ms`.

## Rust authoring

```rust
use octet_extension::{Deserialize, Extension, JsonSchema, ToolResult};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input { name: String }

fn main() -> Result<(), octet_extension::Error> {
    let mut ext = Extension::new();
    ext.tool("hello", "Return a local greeting", |input: Input, call| {
        call.check_cancelled()?;
        Ok(ToolResult::text(format!("Hello, {}!", input.name)))
    })?;
    ext.run()
}
```

Declare path dependency `octet-extension` and pinned `serde` (derive) and
`schemars` in your Cargo manifest, as in the example template. Schemars generates
the actual object input schema. Serde performs typed deserialization; the SDK
also validates the schema before domain dispatch. Use `deny_unknown_fields` to
make the struct/catalog agree. Optional/defaulted fields, nonrecursive nested
structs, homogeneous arrays and supported enum/union schemas work when generation
falls within the subset below. Schemars numeric `format` metadata is removed;
concrete numeric widths are still enforced by Serde. Recursion/references,
validation patterns/formats and unsupported vocabulary fail registration instead
of being silently ignored. Author-controlled custom Serde/Schemars implementations
must describe the same input; the SDK cannot prove arbitrary custom trait code.

The supported schema keywords are `$schema`, `title`, `description`, `default`,
`type`, `properties`, `required`, `additionalProperties`, `items`, `enum`,
`anyOf`, `allOf`, `oneOf`, `minimum`, `maximum`, `minLength`, `maxLength`,
`minItems`, `maxItems`. This is intentionally smaller than the host's input-schema
subset. Root input must be an object. Schema bounds: 64 KiB/tool, depth 32,
4,096 visited nodes; property names at most 256 bytes. The complete catalog must
fit the initialization frame. C/C++ input builders expose only flat strings,
portable signed integers and booleans (64 fields/tool); no JSON parsing is needed
in a callback.

Return `ToolResult::text` for success, `ToolResult::error` or `Error::tool` for an
inspectable domain failure (`is_error: true`). Malformed envelopes/arguments use
JSON-RPC errors, not a fabricated successful result. Rust handler panics become
`-32603`; normal panic diagnostics still go to stderr. C++ exceptions are caught
in its wrapper before the C boundary; raw C callbacks must never unwind/throw or
longjmp. Rust exported C functions catch Rust unwinds, not invalid foreign memory,
abort or allocation failure. See [C ownership contract](../c/README.md).

`CallContext::host_context()` exposes read-only opaque **host-issued** context,
not an owner inferred from model arguments. Context alone conveys no reverse
service authority. This initial C/C++ surface does not expose host context.

## Exact wire and bounded lifecycle

- Only **feature-negotiated API `0.4`** is implemented. Initialization rejects
  `0.1`, `0.2`, and distinct canonical `0.3`, even if a caller retags one field.
  Existing canonical schema/bindings/examples are untouched. See the
  [wire reference](../../docs/extensions/PROTOCOL-REFERENCE.md) and
  [version policy](../../docs/extensions/API-0.4-REFERENCE.md).
- The host must offer required `request_cancellation` and `content_parts`; only
  these are selected. Unsupported required, duplicate/overlapping feature lists,
  invalid concurrency, nonmatching tool declarations and non-tool contributions
  fail explicitly. Unknown optional features are not selected. Concurrency is
  selected as **one**; domain handlers never overlap. No optional service is
  advertised just because the host knows it.
- One bounded reader channel (one queued complete frame) and one admitted worker;
  concurrent excess requests are refused rather than queued unboundedly. One
  stdout mutex serializes complete writes and flushes each frame. Stdout belongs
  exclusively to the protocol; author diagnostics belong on stderr. Do not
  directly print or create background jobs that outlive `run`.
- UTF-8 JSON objects with one LF; ordinary whitespace/order and CRLF are accepted,
  not canonicalized. Cap: 1 MiB excluding LF (a CR counts in the byte cap).
  Reading is bounded **before** allocating beyond the frame. Oversized or
  unterminated frames close the stream. Invalid UTF-8/JSON, duplicate keys,
  ambiguous envelope fields, bad IDs/params are refused; eight malformed-frame
  refusals per process close the stream. Parsed requests cap depth at 32 and
  values at 16,384 nodes. IDs are unsigned u64 or UTF-8 strings up to 256 bytes.
  Tool identifiers follow the actual API 0.4 host's 64-byte ASCII bound, not the
  distinct canonical 0.3 schema's 128-byte tool-name bound.
- Initialization deadline is five seconds. `run` is once per process. Host pipe
  I/O backpressure/OS scheduling cannot be given a portable SDK wall-clock bound;
  the host's initialization/write/shutdown timeouts and process-group kill remain
  final authority. An author handler cannot block protocol reads.
- Cancellation is cooperative, idempotent, and settles the original request with
  `-32800` when it wins the mutex-protected terminal race. An already-selected
  normal result may win instead. Poll `check_cancelled` between effects or use
  interruptible `wait`. Cancellation is **not rollback** or replay permission.
  The actual host enforces cancellation grace for an uncooperative active call.
- Shutdown stops admission, cancels and joins the handler, replies `{}`, flushes,
  and returns. EOF cancels/joins without a shutdown acknowledgement. After 500 ms
  drain expiry with a callback still active, the **executable exits 70**, rather
  than returning into an author that might free live callback data. This is a
  process-authoring library, not a reusable in-process server.
- A duplicate *active* request ID closes the stream with an invalid-request error
  on null ID rather than issuing two conflicting terminals for the original ID.
  IDs may be reused after settlement; cancellation for an unknown/settled ID is
  ignored. Peers must not reuse an ID while an old cancellation is still pending,
  since JSON-RPC cannot distinguish it from cancellation of that newer call.
- Text inputs/results cap at 128 KiB UTF-8 bytes each (schema string lengths count
  Unicode scalars). Results have one explicit text part, `is_error`, and null
  metadata. Worst-case JSON escaping fits the 1 MiB frame. Empty text is valid.

## Concrete missing capabilities

No commands, hooks (including lifecycle/cache-warming/compaction), UI, menus,
renderers, context contributions, CLI flags, notifications, progress, dynamic tool
catalogs, output schemas/structured content, metadata customization, image/audio
artifacts, host-request/reverse services, effects broker convenience APIs,
secrets/approvals, composition, providers/auth, session management, subagents, or
Pi ABI/SDK parity. Requests for unsupported methods return `-32601`; unsupported
manifest contributions fail initialization. Authors who need these services
should use the existing qualified authoring path, not assume this lane implements
it. Do not treat raw OS effects as brokered effects or sandboxed operations.

## Verification and platform limits

```sh
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 cargo test --manifest-path sdk/rust/Cargo.toml --offline --locked --lib
python3 sdk/rust/tests/test_process.py
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 CARGO_TARGET_DIR="$PWD/sdk/rust/target-host" \
  cargo test --manifest-path sdk/rust/host-check/Cargo.toml --offline --locked --lib -- --nocapture
python3 scripts/generate-extension-api-v03.py --check
```

The companion test workspace uses the **actual `octet-agent::ExtensionProcess`**
(start/negotiation/tool decoder, writer cancellation, tombstones and shutdown),
and **`octet-ai::validate_tool_arguments`** for generated schema acceptance,
without a provider/model or full root-workspace test run. Actual catalog/default
policy checks reject disabled and untrusted global-source descriptors before
starting the checked-in relative entrypoint with an exact source grant. This
qualifies the library-level host, not every product frontend policy. It also preserves the
live canonical 0.3 process contract in a private staging copy: exact source
bytes/API/version, with only that copy's host requirement adjusted from the
unchanged `=0.8.0` source pin to `=0.8.2`. This isn't republishing or retagging
canonical 0.3. The companion is test-only and isn't a replacement host. The raw
subprocess tests add malicious frames, exact
frame limits, cooperative/uncooperative shutdown, panic/exception containment,
C registration/result-copy lifetimes, null/type/length accessors and all three
compiled examples. Compilation or headers alone are not qualification.

`SANITIZE=1 OCTET_NATIVE_BIN_DIR=... bash .../build.sh` instruments C/C++ author
and probe code with available compiler ASan/UBSan. The stable Rust static library
is **not sanitizer-instrumented**; this is not whole-runtime sanitizer coverage.
Run the process suite against that directory too. No sanitizer is installed.

The Bash recipe has macOS and Linux linker branches; the candidate evidence is
local macOS arm64. Linux/x86_64/Windows/cross-target builds and MSRV 1.88 execution
are not qualified here. Windows Rust-std import/system-library requirements and
MSVC/GNU ABI selection need their own build recipe and process tests. Binaries
are platform-specific and require matching architecture/libc/OS baseline; there
are no distributed native SDK archives or publication claims.

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
install tools. Direct runtime dependencies are Serde, serde_json, Schemars,
SHA-256 (`sha2`), and Unix no-follow file-open flags (`libc`),
all cached libraries used in this checkout; no host crates are linked
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

### Typed input/output, diagnostics and progress

`tool` keeps its existing typed-input/text-result API. For generated input **and
output** contracts, use the additive `typed_tool::<Input, Output, _>`:

```rust
use octet_extension::{Deserialize, Extension, JsonSchema, Serialize, ToolResult};

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Record {
    name: String,
    enabled: bool,
    samples: Vec<f64>,
    #[serde(default)]
    note: Option<String>,
}

fn main() -> Result<(), octet_extension::Error> {
    let mut ext = Extension::new();
    ext.typed_tool::<Record, Record, _>("echo", "Echo a typed record", |record, call| {
        call.check_cancelled()?;
        ToolResult::structured(record, "Typed record returned")
    })?;
    ext.run()
}
```

`typed_tool` generates `parameters` and `output_schema` together. The handler
returns `ToolResult`; `structured(value, summary)` serializes the value and the
registered output schema **and** Serde output codec are checked before delivery.
A success cannot omit structured content; an error may omit it. Plain-text
summaries are explicit, nonempty, at most 4096 UTF-8 bytes, with no terminal
controls (newline/tab allowed). Structured content is at most 256 KiB, depth 32,
16,384 nodes. Portable integers are ±(2^53−1); nonfinite floats are refused even
inside `Some(NaN)` rather than silently becoming JSON null. Existing `tool` and
C/C++ text results keep their old bounds and semantics.

Typed records are closed. `Option<T>` accepts omission or explicit null as
`None`; `#[serde(default)]` supplies a declared default for omitted fields only,
not a coercion of explicit null. Nonrecursive records, homogeneous lists,
primitive scalars and schema-supported enums/tagged variants are supported;
untyped JSON, maps, tuples, recursion and unsupported schema vocabulary fail
registration. Rust numeric widths still constrain codecs; generated f32 schemas
also bound finite range to refuse narrowing overflow before handler entry. Custom Serde/Schemars
implementations must describe the same contract. The shared cross-SDK fixture
is [typed-values-v1](../conformance/README.md).

`result.with_diagnostics(vec![...])` attaches
validated `Diagnostic` records in reserved `metadata.octet_diagnostics_v1` and
appends the shared bounded plain-text projection (first eight, ≤4096 bytes).
Use `Diagnostic::new(Severity::Error, "solver.failed", "No convergence")` and
`diagnostic::{Location, Source, Span, Related, Fix, Edit, Attachment}` for optional
revision-bound context. Omit unavailable locations. Fixes are suggestions, not
filesystem authority; references do not create objects or grant access.

When the host offers `request_progress`, the SDK selects it. During the call,
`call.progress("Working")?` or `progress_status(message, current, total, unit)`
emits a bounded status with a monotonically increasing request-local sequence.
Unnegotiated, cancelled or settled calls refuse progress. These statuses are
ephemeral, never structured output or lossless transport, and never extend the
host deadline. Cancellation and terminal selection retain their existing race.

`CallContext::host_context()` exposes read-only opaque **host-issued** context,
not an owner inferred from model arguments. Context alone conveys no reverse
service authority. This initial C/C++ surface does not expose host context.

### Native resources and operations

Declare `impl ResourceType for Counter { const TYPE_ID: &'static str = "hello.Counter"; }`
once; fields typed `Resource<Counter>` generate a closed nominal schema and every
input/output slot automatically. `typed_tool` adds an operation descriptor only
when such slots exist. `operation::<I, O, _>(name, description, receiver, handler)`
opts into discovery for plain or resource-bearing operations and optionally names
a presentation-only receiver pointer; **all arguments remain ordinary input**.
The operation id is the tool name. Slots must be fixed nested object properties;
resource arrays, unions/nullables, and root-resource outputs fail registration.

`call.export(native)?` transfers ownership into the bounded process-local registry
and requests a provisional host identity. Return it in the declared structured
output; only complete host result admission activates it. Native objects are never
serialized. `call.with_resource(&input.counter, |counter| { ... })?` borrows an
input or newly exported object exclusively for a callback, on the active handler
lane. References are owner/generation/type checked before domain dispatch;
cloning `Resource<T>` clones identity only. Native `T` must be `Send + 'static`.

`call.release(&reference)?` retires an unpinned host identity and reports separate
cleanup status; trying to release a current pinned input correctly fails busy.
Host `resource/dispose` immediately retires local identities, then serializes
cleanup with the existing native execution lane. Override `ResourceType::dispose(self)`
for fallible cleanup; the default drops the value. Panic/error cannot resurrect
an identity. Cancellation never eagerly frees an active native borrow. Shutdown
also drains native values on that lane within the existing bounded drain policy.
Do not spawn background jobs with resource calls: these helpers require the active
handler thread. Host cleanup and process fencing remain authoritative.

Resource-bearing catalogs require negotiated `resource_refs_v1` plus
`operation_descriptors_v1`, with exact v1 limits (256 records, 32 registrations per
parent). Plain explicitly annotated operations require only operation descriptors.
Ordinary tools and C/C++ ABI1 require no resource setup or new callbacks. Reverse
requests share the existing reader/stdout writer, with 32 pending slots, 65,536
nonreused child IDs per process, a 30-second ceiling, and cooperative cancellation.
A late registration response cannot publish or restore a locally disposed value;
unknown disposal identities report failed, never falsely acknowledge cleanup.

Runnable two-tool example: [rust/resources.rs](../../examples/extensions/native-hello/rust/resources.rs)
(Cargo example `resource-hello`). `examples/extensions/native-hello/build.sh`
stages a package-local executable for its checked-in `resources/extension.toml`.
That manifest declares `counter_create` and `counter_add`; start it through the
API 0.4 host with a session-owned context. No hand-written schema/slot list or
second transport is needed.

### Immutable binary data

Fields typed `BlobRef` generate the closed blob schema and codec once (opaque
`$blob`, portable `bytes`, SHA-256 `digest`, and `media_type`). The reference is
metadata, **not read authority**. When a typed catalog contains BlobRefs, the SDK
requires `bulk_objects_v1` and the host's configured `local-file.v1` profile.
Ordinary tools do not negotiate it. This helper profile currently requires Unix;
unsupported platforms/profiles fail explicitly without a bytes-in-JSON fallback.

- `call.write_blob(capacity, media_type, |writer| writer.write_all(bytes))?`
  reserves a bounded ticket, streams/hash-checks data, closes its file, and commits
  a provisional BlobRef. Return that reference in successful structured content;
  error/cancelled/invalid results do not publish it. An active failed write releases
  its ticket; host parent retirement cleans cancelled/abandoned transfers.
- `call.read_blob(&reference, |reader| std::io::copy(reader, &mut sink))?`
  obtains a fresh authorized read lease, bounds reads to the descriptor length,
  verifies the entire SHA-256 (draining unread data), and releases the lease before
  returning. Callback effects are not rolled back on integrity failure: do not
  publish irreversible effects before the helper succeeds.

Callbacks receive only `Read`/`Write`, never paths, leases, tickets or backing
storage. Internal chunks are at most 64 KiB; negotiated capacities are enforced
before writing, cancellation is checked between chunks, and no payload byte array
or base64 crosses JSON-RPC. Locators must be single relative filenames inside the
negotiated directory; no-follow/nonblocking opens refuse symlinks, nonregular files
and hardlinks. IO errors expose bounded generic messages, not private paths.
The host re-verifies snapshots and owns session retention/durable recovery; SDK
helpers cannot restore grants from a transcript or resurrect native resources.

See runnable [rust/blobs.rs](../../examples/extensions/native-hello/rust/blobs.rs)
(Cargo example `bulk-hello`; manifest tools `blob_save`, `blob_size`). The same
build script stages its package-local executable for `blobs/extension.toml`. Its host must
supply configured bulk storage and a session-owned execution context. These are
Rust additions only; C/C++ ABI1 remains the existing text-result authoring API.

## Exact wire and bounded lifecycle

- Only **feature-negotiated API `0.4`** is implemented. Initialization rejects
  `0.1`, `0.2`, and distinct canonical `0.3`, even if a caller retags one field.
  Existing canonical schema/bindings/examples are untouched. See the
  [wire reference](../../docs/extensions/PROTOCOL-REFERENCE.md) and
  [version policy](../../docs/extensions/API-0.4-REFERENCE.md).
- The host must offer required `request_cancellation` and `content_parts`;
  `request_progress` is additionally selected only when offered. Resource/operation
  features are selected only for author-declared operations. Unsupported required, duplicate/overlapping feature lists,
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
  Unicode scalars). Results have one explicit text part and `is_error`; metadata
  is null unless diagnostics are attached. Typed tools additionally carry
  schema-validated `structured_content`. Full-frame bounds are checked before
  delivery. Empty text is valid for the retained lower-level API.

## Concrete missing capabilities

No commands, hooks (including lifecycle/cache-warming/compaction), UI, menus,
renderers, context contributions, CLI flags, notifications, dynamic tool
catalogs, arbitrary metadata customization, image/audio
artifacts, general host-request/reverse services (beyond native resource and bulk helpers), effects broker convenience APIs,
secrets/approvals, composition, providers/auth, session management, subagents, or
Pi ABI/SDK parity. Requests for unsupported methods return `-32601`; unsupported
manifest contributions fail initialization. Authors who need these services
should use the existing qualified authoring path, not assume this lane implements
it. Do not treat raw OS effects as brokered effects or sandboxed operations.

## Verification and platform limits

```sh
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 cargo test --manifest-path sdk/rust/Cargo.toml --offline --locked --lib
python3 sdk/rust/tests/test_process.py
CARGO_BUILD_JOBS=1 cargo build --manifest-path sdk/rust/Cargo.toml --offline --locked --examples
python3 sdk/rust/tests/test_typed_process.py
python3 sdk/rust/tests/test_resource_process.py
python3 sdk/rust/tests/test_bulk_process.py
CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 CARGO_TARGET_DIR="$PWD/sdk/rust/target" \
  CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo test --manifest-path sdk/rust/host-check/Cargo.toml --offline --locked --lib -- --nocapture
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
compiled examples. The separate `typed-probe` executable and host-check tests
consume shared schema fixtures, assert invalid inputs leave the child call log
unchanged, refuse invalid outputs/diagnostics, retain structured results, and
exercise negotiated progress and barrier-observed cooperative cancellation. Compilation or headers alone are not qualification.

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

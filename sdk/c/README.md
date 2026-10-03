# C executable-extension authoring

C ABI **1** wraps [the same Rust API 0.4 runtime](../rust/README.md). A C author
provides flat typed fields and a callback; no JSON parser, RPC envelope, input
loop, cancellation reader or shutdown plumbing is needed. This is a supervised
**executable** SDK, not C plugins loaded inside octet.

See [`include/octet.h`](include/octet.h) for the full ownership, threading,
UTF-8 byte-length, null, bounds, error and no-unwind contracts. In brief:

- Allocate with a null handle slot; `free(&handle)` nulls it. No handle aliases or
  calls on the handle during `run`. `run` is once per process and consumes tools.
- Tool/field definition bytes are copied at registration. Callback/user data
  stay alive through `run`; domain callbacks are serialized on one worker thread.
- Argument strings are borrowed through callback return; result text is **copied**
  immediately. UTF-8 slices are not NUL-terminated; embedded NUL is data.
- Null+zero slice/field arrays are valid. Null+positive length, oversized lengths,
  invalid UTF-8, invalid fields, duplicates and bad result flags fail explicitly.
  Non-null foreign pointers must still designate valid aligned live allocations:
  arbitrary pointer validity, stale handles and double frees cannot be recovered.
- Accessor output slots are zeroed on failure. `MISSING` means absent; `TYPE`
  includes present JSON null. Required/type/range/length input checks run before
  the callback. Status codes are ABI-local, not wire JSON-RPC codes.
- Supply exactly one result and return `OCTET_OK`. Non-OK callback status produces
  an inspectable tool error; `OCTET_CANCELLED` maps to original-request `-32800`.
  Poll `octet_check_cancelled` or use `octet_wait` between effects. No rollback.
- Raw C callbacks must never throw/unwind/longjmp across the ABI. Rust catches its
  own panics at exported functions. Invalid memory/abort/OOM aren't caught panics.
  C++ authors should use [the exception-containing wrapper](../cpp/README.md).
- Shutdown/EOF cancels and joins. A callback exceeding the 500 ms drain causes
  executable exit 70, not a return with user data still borrowed. Pipe backpressure
  is ultimately bounded by the actual host supervisor, not a C library promise.

C builders support up to 64 flat fields/tool: bounded strings, portable signed
integers (-9007199254740991..9007199254740991), booleans, required/optional fields.
String schema lengths count Unicode scalars; a separate 128 KiB UTF-8 byte cap
applies. Tool names follow the actual API 0.4 host's 64-byte ASCII bound.
Nested structs, array/union builders, defaults and host context aren't exposed
through this initial C ABI. All languages share its tool-only feature limitations;
see the [complete missing-capability list](../rust/README.md#concrete-missing-capabilities).

## Local source build

From the checkout root:

```sh
bash examples/extensions/native-hello/build.sh
# Reusable single-source C build template (normal make variable overrides):
make -C sdk/c OCTET_ROOT="$(pwd)"
```

`Makefile` accepts `MAIN`, `OUTPUT`, `TARGET_DIR`, `CC`, `CFLAGS`, `LDFLAGS`.
It builds the Rust static archive offline/locked, then links a C11 executable.
See [the runnable example and generated local manifests](../../examples/extensions/native-hello/README.md).
No global install, registry/publication, enablement or host execution occurs
at build time. A Rust toolchain and matching target C linker are required.

The local macOS arm64 candidate checks compile/link/run real C processes, exercise
null/length/UTF-8 cases and immediate definition/result-copy lifetimes, and can
instrument C author/probe code using compiler ASan/UBSan. Stable Rust itself is
not sanitizer-instrumented. Linux's linker branch is provided but unqualified;
Windows/MSVC/GNU and cross-target ABI/link requirements remain unqualified. No
precompiled SDK archive availability is claimed.

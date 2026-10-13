# C++ executable-extension authoring

`include/octet.hpp` is a header-only **C++17** wrapper over [C ABI 1](../c/README.md)
and [the same Rust API 0.4 runtime](../rust/README.md), not a second protocol
implementation. It handles ownership, callable storage, argument copying,
result copying and exception containment. No JSON/RPC or lifecycle code belongs
in an author function.

```cpp
#include <octet.hpp>
int main() {
    octet::extension ext;
    ext.tool("hello", "Return a local greeting", {
        octet::field::string("name", 256)
    }, [](octet::call &call) {
        call.check_cancelled();
        return octet::result::text("Hello, " + call.string("name") + "!");
    });
    ext.run();
}
```

In production author `main`, catch `std::exception` and report on **stderr**;
see [the complete one-source example](../../examples/extensions/native-hello/cpp/main.cpp).
Domain handler exceptions are already caught by the wrapper and become tool
errors; cancellation stays cancellation. No C++ exception crosses the C ABI.

`call.string` returns an owned `std::string`, including embedded NUL bytes.
`integer`/`boolean` read typed fields; `integer_or` distinguishes absent from
wrong type, `wait` is interruptible. `result::text`/`result::error` supply one
text-only terminal. Flat `field::string`/`integer`/`boolean` builders generate
actual input schemas and validate before callbacks. The extension owns its
lambda/captures through `run`; copy/move is disabled to keep callback ownership
simple. Do not retain a `call` reference/pointer, destroy the extension inside
its callback, print to stdout, or spawn jobs using callback data after return.

`run` consumes registrations and is once per process. Shutdown/EOF cancel/join
callbacks; an uncooperative callback at 500 ms drain exits the **process** with
70 rather than permitting a use-after-free after `run` returns. A native SDK
status error is explicit; unsupported hooks/UI/commands/flags and required
features fail initialization. This is tool-only authoring, **not full SDK/Pi
feature parity**. See [missing capabilities and bounds](../rust/README.md).

## Build template

```sh
# From the repository root:
bash examples/extensions/native-hello/build.sh
make -C sdk/cpp OCTET_ROOT="$(pwd)"
```

`Makefile` accepts `MAIN`, `OUTPUT`, `TARGET_DIR`, `CXX`, `CXXFLAGS`, `LDFLAGS`;
it builds and links the same Rust archive as C. Rust/Cargo, a matching target
C++17 toolchain/linker and cached dependencies are required. No runtime registry,
global install or toolchain-free source execution is promised. Resulting binaries
are target/OS-specific. Local macOS arm64 process checks include capture lifetime,
exception containment, cancellation, shutdown and available C/C++ ASan/UBSan
instrumentation; stable Rust is not sanitizer-instrumented. Linux/Windows/cross-
target builds need their own qualification.

# api-v03-minimal executable extension

This is the retained canonical API `0.3` reference extension. It is an ordinary local
process, not a Python SDK runtime: `extension.py` uses only the Python standard
library and speaks the canonical JSON-RPC wire directly.

## Prerequisites

- source-built octet **0.8.0 RC** with executable extensions enabled by policy;
- Python **3.9 or newer** available as `python3` (no third-party packages);
- a host API **0.3** contract. The manifest's `requires_octet = "=0.8.0"`
  intentionally rejects a different host version.

The process selects only the required API `0.3` capabilities and methods:
`core`, `content_parts`, `request_cancellation`, `tool_call`, `initialize`,
`tool/call`, `$/cancelRequest`, and `shutdown`. It does not claim optional or
deferred services.

## Install and enable

Copy this directory without renaming it. Discovery requires the directory name
to match `extension.toml`'s `name`:

```console
mkdir -p .octet/extensions
cp -R examples/extensions/api-v03-minimal .octet/extensions/
```

Executable extensions are disabled by default. In a reviewed full-access
invocation, enable and explicitly trust this extension:

```console
octet --enable-extension api-v03-minimal --trust-extension api-v03-minimal
```

`--safe-mode` never starts executable extensions. Capability declarations are
consent metadata, not an OS sandbox; this process has the user's ordinary OS
authority. Use a separately isolated workspace for untrusted code. The
native-host protocol `1` is a separate embedding interface and does not start
this process.

## Behavior and expected output

The host sends a canonical `initialize` request. The extension validates the
exact API/schema/encoding, rejects unknown or deferred capabilities and methods,
and returns one `echo` tool. A call such as:

```json
{"arguments":{"text":"hello from API 0.3"},"context":{},"name":"echo"}
```

returns a canonical result containing text content, structured content, and
metadata:

```json
{"content":[{"text":"hello from API 0.3","type":"text"}],"is_error":false,"metadata":{"delay_ms":0},"structured_content":{"text":"hello from API 0.3"}}
```

`delay_ms` is optional and bounded to `0..5000`. It exists to make cooperative
cancellation observable. While a delayed call is active, the host may send the
`$/cancelRequest` notification with the call's JSON-RPC ID. The extension then
returns the exact API error `-32800` / `request cancelled`; cancellation is
cooperative and has no rollback meaning. `shutdown` cancels active calls,
returns `{"terminal":"shutdown"}`, flushes stdout, and exits.

Frames are UTF-8 canonical JSON followed by exactly one LF. Stdout is reserved
for protocol frames; diagnostics go to stderr. Unknown methods return
`-32601` / `unknown or unnegotiated method`. An API, schema, capability, or
method-contract mismatch fails explicitly rather than being silently upgraded.

## Verify without a host build

From this directory, run the process-level Python checks:

```console
python3 -m unittest discover -s . -p 'test_*.py'
```

The checks exercise canonical negotiation, a real tool call, unknown-envelope
and unknown-method failures, version/capability mismatch failures,
cooperative cancellation, and graceful shutdown. From the repository root, the
generated API and cross-language fixture check remains:

```console
python3 scripts/generate-extension-api-v03.py --check
```

For host-side qualification, run the `octet-agent` crate's `api_v03_runnable`
integration test from the repository's permitted build/test slot. It verifies
this source manifest's exact `=0.8.0` runtime pin and rejects a nonmatching host.
It stages a **private temporary copy**, preserves the executable source bytes,
extension version `0.1.0`, and API `0.3`, and sets only that copy's runtime
requirement to the current crate's exact Cargo version. The ordinary host runs
negotiation, tool, cancellation, and shutdown checks against the staged copy.

A passing staged test is source/wire qualification, **not** evidence of published
0.8.0 assets or cross-version installability. The checked-in manifest and runtime
pin validation are not changed by the test.

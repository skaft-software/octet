# octet-codemode

Compose host tools with JavaScript in a **sandboxed QuickJS/WASM runtime**:
one Rust executable with the vendored `quickjs-wasi` 3.6.2 engine, Pi's
`@earendil-works/pi-codemode` 1.0.0 prelude (MIT) and Octet's discovery
implementation embedded at compile time. **No Node.js and no Python are
required at runtime**, and no runtime JavaScript is read from the checkout.
This API `0.4` bundle pins octet `0.9.0`.

## Install and enable

This bundle joins `extensions/release-catalog.txt`; its release asset is
`octet-codemode-0.9.0.tar.gz`. Once that asset is published:

```sh
octet extension install octet-codemode
octet --enable-extension octet-codemode --codemode-mode on
```

For an unpublished source checkout, copy this complete directory to
`~/.octet/extensions/octet-codemode/` and explicitly enable it. Installation
alone does not enable execution. Follow the host's extension trust/grant policy;
the trusted Rust launcher is an ordinary OS process, **not an OS sandbox**.
Only guest JavaScript is isolated. `--no-process`/`--no-shell` do not apply to
this bundle (`process = false` in `extension.toml`).

The entrypoint is `bin/codemode`. It resolves, in order, an explicit
`OCTET_CODEMODE_BINARY`, a prebuilt `bin/octet-codemode` shipped beside it, an
in-tree `target/{release,debug}/octet-codemode`, an installed `octet-codemode`
on `PATH`, and finally builds the crate once with `cargo --locked --release`
into a private cache. Only that last step needs Rust, and it needs neither
Node nor Python:

```sh
cargo build --locked --release --manifest-path extensions/octet-codemode/Cargo.toml
```

`--codemode-mode on` keeps direct tools and adds `codemode`. `only` advertises
composition tools alone, without widening the nested tool allowlist.
`--codemode-inline-budget 0..16000` controls estimated inline declaration tokens
(default 3000); omitted/deferred tools remain discoverable. Flags are resolved
at startup. `/codemode [status|help]` and the extension menu show limits and help;
neither grants execution authority or starts a VM.

## Engine and isolation

- **Default engine: Wasmi 0.46 + the vendored QuickJS-WASI 3.6.2 reactor**
  (pure Rust, no C toolchain at runtime, no WASI filesystem/network/preopens).
- **Optional native lane: rquickjs 0.14 with QuickJS-NG 0.16.2**, selected
  explicitly with `serve --engine native`. It is not the default and shares the
  same adapter, limits and prelude.
- One **warm disposable runner** per extension generation. The WASM module is
  compiled and validated once, the invariant guest program is compiled to
  QuickJS bytecode once, and every script then gets a **fresh isolated Wasmi
  store** (or realm) that is discarded afterwards: no global, heap object,
  pending promise or store snapshot survives into the next script. That is
  asserted by `tests/test_runner.py` and `tests/test_adapter.py`.
- Scripts are bounded by the engine interrupt at the script deadline. A wedged
  VM that never reaches an interrupt check is killed by the runner watchdog as
  the fallback; cancellation kills and reaps the runner, and the next script
  gets a new one.

## JavaScript contract

The `codemode` tool accepts exactly `{ "code": "..." }`. Its source is a
JavaScript **async function body**, not TypeScript: top-level `await` and
`return` work. An optional first line sets output and timeout preferences:

```js
// @options: {"max_output_tokens": 2000, "timeout_ms": 15000}
const results = await Promise.allSettled([
  tools.read({path: "README.md"}),
  tools.read({path: "Cargo.toml"}),
]);
return results.filter(r => r.status === "fulfilled")
  .map(r => ({path: r.value.path, lines: r.value.total_lines}));
```

Only tools actually enabled and allowed by the host are available. Core
`read` and `bash` expose structured programmatic results while their
ordinary direct presentation stays unchanged. Declared extension
`output_schema` results are JSON; schema-less tools return text, **never
implicitly parsed JSON**. Images/audio read by a nested tool are not implicitly
published to the model.

Available globals:

- `tools.<alias>(arguments)`, `ALL_TOOLS`;
- `searchTools(query, {limit?, namespace?})` (BM25; default 8),
  `describeTool(name)`, `describeNamespace(name)`;
- `text(value)`, `image(dataUrlOrImageBlock)`, `console.log/info/warn/error/debug`,
  `exit()`;
- `store(key, value)`, `load(key)`.

Pi's identifier normalization replaces invalid identifier characters with `_`.
Raw names and aliases use first-write-wins bindings; collisions never authorize
a different tool. Tool failures reject promises; use `Promise.allSettled` for
partial success. Only four host-declared safe observations can overlap; writes,
shell commands and extension effects remain exclusive and run through ordinary
schema validation, effects, approvals and before/after hooks.

There is **no** `process`, `require`, filesystem, `fetch`, socket, subprocess,
timer, import, or `models` namespace in the guest. Model/classifier/image tools
can be called only if separately enabled and brokered; this chat-only host does
not supply Pi's models helper. Guest code never receives API keys or host scratch
paths as an authority-bearing API.

## Interactive presentation

The ANSI TUI highlights a multiline JavaScript preview; Ctrl+O expands the full
script. Inside Tern, the full script is a wrapping native code rail. Both show a
bounded literal output preview (five lines / 600 Unicode characters); Ctrl+O
reveals the retained output. Preview elision does not discard captured data or
change the model-visible result, tool approvals or execution. Native output has
its own code surface rather than a raw generic tool-card dump.

## Performance

`bench/benchmark.py` drives the same fixture host and the same scripts through
this extension and through the previous Node/Pi bundle (`@earendil-works/pi-codemode`
1.0.0 + `quickjs-wasi` 3.6.2, the shipped launcher at revision `901e12eb`),
30 samples per case, extension process tree peak RSS (`VmHWM`) and CPU from
`/proc`. Raw samples: [`bench/results.json`](bench/results.json).

| Case (30 samples) | Rust (Wasmi, release) | Node/Pi 1.0.0 | Result |
|---|---:|---:|---|
| First script in a fresh process (cold) | 53.9 ms | 47.4 ms | 1.14x slower |
| Subsequent script in a warm process | **19.0 ms** | 28.1 ms | **1.5x faster** |
| One script making 50 brokered tool calls | 44.1 ms | 32.0 ms | 1.4x slower |
| Peak RSS, extension process tree | **15.8 MiB** | 99.0 MiB | **6.3x lower** |
| CPU, whole measured run | 2.04 s | 2.28 s | ~equal |

Read honestly: the warm path, which is what a session pays repeatedly, is
faster than Pi's runtime, and memory is a fraction. The cold path pays a
one-time WASM + guest-bytecode compile; the 50-call script is slower because each
round trip crosses the runner pipe plus this adapter's per-call bookkeeping
(task, limiter, cancel scope) on top of Wasmi interpreting the guest. The native
lane removes the interpreter cost for callers who opt into it
(`serve --engine native`: ~2.5 ms warm setup).

Note on the reference: the shipped Node/Pi bundle rejects the 0.9.0 feature
offer (it requires the older key set), so it is measured on the offer it
accepts, while this extension runs the real 0.9.0 offer. The bundled Pi runtime
is the behaviour reference, not a claim of bit-identical ECMAScript output.

## Persistence, output and limits

- Code: at most 65,536 Unicode characters. VM heap: 256 MiB.
- Local total deadline: 25 seconds; host ceiling: 30 seconds. Lower host or
  script limits win. A script cannot raise a host ceiling.
- At most 256 nested calls per parent. Unawaited/queued calls are cancelled on
  completion, abort, shutdown or transport loss; completed effects are **not
  rolled back**, including when a script throws.
- Text output: default 10,000 estimated tokens, including a valid zero-token
  preference. Visible head/tail plus notices stay within 50 KiB. Full truncated
  UTF-8 text goes to private scratch (32 files/64 MiB, oldest first); the returned
  path can be read with an enabled `read` tool. Capture is bounded at 16 MiB,
  4096 parts and 64 images.
- `image()` accepts base64 PNG/JPEG/GIF/WebP and requires negotiated host
  artifacts. The host validates and owns publication; failure prevents store
  commit.
- Store writes commit once, only after successful, noncancelled execution.
  Each value is limited to 256 KiB encoded JSON; the branch store to 1 MiB.
  Store ancestry survives reopen, fork and compaction without becoming chat
  context. It is not a secret vault or a shared cross-session database.
- Nested invocation receipts are private, synced session metadata. They record
  argument digests and policy/outcomes, not ordinary raw nested output. Provisional
  delivery requires an exact bounded private receipt before acknowledgment.
- Completed nested model usage remains accounted even after script failure or
  cancellation. The host does not invent pricing or attribute it to the chat
  model. Under hard session token/cost ceilings, tools without a host-owned
  unmetered contract or authoritative pre-execution bounds are refused.
- Interrupted scripts are unsafe to replay automatically. Reload replaces the
  process generation; stale, foreign, command and hook parents cannot compose.

Large host context/result JSON uses host-issued scratch sidecars, never a larger
wire frame: flat random basenames, regular no-follow files, exact size at most
8 MiB, SHA256 verification and cleanup. See the generic
[`tool_composition_v1` contract](../../docs/extensions/PROTOCOL-REFERENCE.md#225-compositioncontext-compositioncall-compositionstore-api-04-feature-tool_composition_v1).

## Verification and provenance

```sh
cargo test --locked                      # 16 Rust unit tests
python3 -m unittest discover -s tests    # 59 real-process tests, both engines
python3 vendor/regenerate.py --check     # offline vendored-bytes check
python3 bench/benchmark.py --samples 30  # the table above
# Native host transport (from the repository root):
# cargo test -p octet-agent codemode_bundle_runs_ -- --ignored
```

`tests/test_runner.py` drives the warm runner protocol directly (prelude
parity, async settlement, discovery/BM25, heap/stack limits, timeouts, warm
reuse and per-script isolation). `tests/test_adapter.py` speaks the real API
`0.4` stdio wire with `PATH` empty (tool composition, sidecars, artifacts,
store commits, cancellation, output limits, warm-runner lifecycle).
`tests/test_bundle.py` packages the release archive twice and verifies the
extracted bundle starts with no Node or Python.

`vendor/regenerate.py` reproduces the retained vendored bytes offline from the
checked-in npm archives (SHA512 integrity, SHA256 per file) and fails on drift.
Only the files this extension embeds are retained: the Pi prelude, identifier
and declaration sources, the grammar source its unit test pins, `quickjs.wasm`
and the complete license set. Node-only loaders, declarations and source maps
are not shipped and no runtime code is patched. See
[third-party notices](THIRD_PARTY_NOTICES.md),
[Pi's MIT license](vendor/pi-codemode/LICENSE), and
[QuickJS WASI's MIT license](vendor/quickjs-wasi/LICENSE).

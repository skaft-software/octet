# octet-codemode

Compose host tools with JavaScript in Pi's **actual QuickJS/WASM runtime**:
`@earendil-works/pi-codemode` 1.0.0 and `quickjs-wasi` 3.6.2, vendored offline
under their MIT licenses, with runtime-component notices retained. This API `0.4`
bundle pins octet `0.8.2` and requires
**Python 3.11+ and Node.js 22.19+** on PATH. No npm install, download, credentials,
or setup command is needed to execute a script.

## Install and enable

This bundle joins `extensions/release-catalog.txt`; its release asset is
`octet-codemode-0.8.2.tar.gz`. Once that asset is published:

```sh
octet extension install octet-codemode
octet --enable-extension octet-codemode --codemode-mode on
```

For an unpublished source checkout, copy this complete directory to
`~/.octet/extensions/octet-codemode/` and explicitly enable it. Installation
alone does not enable execution. Follow the host's extension trust/grant policy;
the trusted Node launcher is an ordinary OS process, **not an OS sandbox**.
Only guest JavaScript is isolated. `--no-process`/`--no-shell` also prevent the
launcher from starting.

`--codemode-mode on` keeps direct tools and adds `codemode`. `only` advertises
composition tools alone, without widening the nested tool allowlist.
`--codemode-inline-budget 0..16000` controls estimated inline declaration tokens
(default 3000); omitted/deferred tools remain discoverable. Flags are resolved
at startup. `/codemode [status|help]` and the extension menu show limits and help;
neither grants execution authority or starts a VM.

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
`read`, `search`, and `bash` expose structured programmatic results while their
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

## Offline verification and provenance

```sh
python3 vendor/regenerate.py --check
python3 -m unittest discover -s tests
# Optional Rust-to-Node/WASM integration (from the repository root):
# cargo test -p octet-agent codemode_bundle_runs_vendored_vm_through_host_transport -- --ignored
```

The Python catalog adapter runs real Pi/WASM, cancellation, stdio, sidecar,
artifact, output and deterministic packaging tests; it fails rather than silently
skipping if Node is absent. `vendor/PROVENANCE.json` pins npm SHA512 integrity,
SHA256, package versions, Pi's published Git head and the missing npm license
supplied from that exact revision. `vendor/SHA256SUMS` covers every retained file.
`python3 vendor/regenerate.py` reproduces the vendor tree **offline** from the
checked-in archives. The sole runtime patch relocates Pi's QuickJS import to a
relative bundled path; the VM and guest prelude are upstream. Optional native
QuickJS `.so` extensions are not loaded or extracted. Supplemental QuickJS-NG,
WASI/LLVM, and archived-module license notices are pinned and reproduced too.

See [third-party notices](THIRD_PARTY_NOTICES.md),
[Pi's MIT license](vendor/pi-codemode/LICENSE), and
[QuickJS WASI's MIT license](vendor/quickjs-wasi/LICENSE).

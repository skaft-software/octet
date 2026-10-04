# Source SDK parity: implementation, reachability, evidence

This is a source inventory, **not full SDK/Pi parity or a release qualification**.
Current authoring uses feature-negotiated API `0.4`; generated canonical `0.3`
bindings are a separate wire, not an executable runtime. Package versions do not
change wire versions. No registry publication, installed binary, provider turn,
frontend or cross-platform qualification follows from this table.

**Implemented** means a public author API has source implementation. **Reachable**
means an author can get to that implementation through that language's public
surface, with the host's required negotiation/owner/service binding. **Native
coverage** identifies production `ExtensionProcess` tests, not declaration checks,
raw JSONL peers, successful compilation, or a claim those tests were run here.

## Author-facing implementation

| Capability | Python | Rust | TypeScript / JavaScript | C ABI 1 | C++17 wrapper |
|---|---|---|---|---|---|
| Executable API 0.4 runtime, static tools, cancellation/shutdown | Implemented | Implemented | Implemented | Reachable through Rust runtime | Reachable through C ABI |
| Input schema/codec | Annotations/dataclasses or explicit schema | Serde/Schemars | Explicit literal schema with TS inference | Flat string/integer/boolean fields only | Same flat C fields; copied values |
| Structured output with output schema and explicit text | `typed_tool`, explicit result API | `typed_tool`, `ToolResult::structured` | `typedTool`, or `tool` with `outputSchema` | **Not exposed**; text only | **Not exposed**; text only |
| Typed diagnostic metadata + bounded text projection | Implemented | Implemented | Implemented via result `diagnostics` and exported validators | Not exposed | Not exposed |
| Progress emission | Implemented | Implemented | Implemented | Not exposed (runtime negotiation is not an author API) | Not exposed |
| Commands | Implemented | Not implemented | Implemented | Not exposed | Not exposed |
| Hooks | Implemented; hook-specific boundaries | Not implemented | Bounded named hook registration/results | Not exposed | Not exposed |
| Native ResourceRef / generated operation slots / cleanup | `Resource[T]`, export/dispose | `Resource<T>`, export/borrow/dispose | `resourceType`, schema-derived slots, export/resolve/dispose | Not exposed | Not exposed |
| BlobRef / immutable bulk | Descriptor + bounded byte helpers | Descriptor + streaming Read/Write helpers | Descriptor/validation + allowlisted low-level reverse requests; **no secure file I/O convenience helper** | Not exposed | Not exposed |
| Image/audio artifact results | Implemented | Not implemented | Inline `publishArtifact` + media parts | Not exposed | Not exposed |
| General reverse services | Named host helpers | Resource/bulk only | Allowlisted active-parent `context.request`; not arbitrary RPC | Not exposed | Not exposed |
| Dynamic catalogs, UI, lifecycle subscriptions | Existing Python surfaces, each gated | Not implemented | Not implemented by this package | Not exposed | Not exposed |
| Retained canonical API 0.3 bindings | Separate generated module; not `Extension` runtime | Separate host contract, not this SDK runtime | Separate generated package; not process package | No author runtime | No author runtime |

Implementation references: [Python](../python/README.md),
[Rust](../rust/README.md), [TS process source](../typescript/process/index.mjs),
[resource helper](../typescript/process/resources.mjs),
[hook/service allowlist](../typescript/process/hooks.mjs),
[value validators](../typescript/process/values.mjs), [C](../c/README.md),
[C++](../cpp/README.md). Rust implementation availability does **not** establish
C/C++ ABI reachability. TS resource schemas must retain their declaration identity;
copying raw schema JSON is not a new resource declaration.

## Reachability and semantic differences

- All executable extensions require reviewed discovery, explicit enablement and
  host startup authority. Capability metadata is not a syscall sandbox.
- Resource operations need both resource/descriptor features and a live owned
  tool parent. Registration is provisional until host result admission. Disposal
  completion/failure is independent of retirement; failure never revives a token.
- Bulk needs configured host storage and `local-file.v1`. TS exposing reverse
  methods is not parity with Python/Rust secure transfer helpers. Blob identity
  alone is not authorization; SDKs do not own durable host retention.
- TS reverse requests attach the active host parent; callers cannot supply
  `parent_request_id`, `resource_owner`, or `session_leaf`. A hook context does
  not grant general service authority. Private session append needs the actual
  host-issued leaf and consumer. Retained-owner/session-control/agent services
  are not in this package's request allowlist.
- Artifacts need negotiation, a session-owned parent and host verification. The
  TS helper is inline-only, at most 256 KiB; raw result parts do not mint access.
- Diagnostic structure/summary is shared; source/ref authorization and fix
  application remain host-owned. Diagnostics do not activate provisional refs.
- Optional/null/default behavior is not identical syntax: Python omission needs
  a dataclass default, Rust follows supported Serde defaults/Option semantics,
  and TS follows explicit `required` fields plus handler defaults. The supported
  schema subsets differ; TS does not claim Python/Rust general union support.
- The TS `typedTool` convenience callback returns a domain value; attaching
  diagnostics/media uses the lower-level schema-declaring `tool` result envelope.
  That is reachable typed output, not identical convenience syntax across SDKs.

## Production-host coverage inventory (not execution results)

| Language | Existing native source coverage | Added focused coverage | Unqualified/gaps |
|---|---|---|---|
| Python | `examples/extensions/python-single-file/host-smoke/tests/typed.rs`: typed/default/null/refusal/diagnostic/progress/cancellation paths | None in this change | 43 native cases passed (`full-python-r2`); not full surface parity |
| Rust | `sdk/rust/host-check/src/{lib,typed_tests,resource_tests,bulk_tests}.rs` and nested matrices: actual SDK executables, typed values, resource lifecycles, bulk | None in this change | Commands/hooks/media/general reverse services absent, not merely untested |
| TS/JS | `sdk/typescript/host-smoke/tests/native_typed.rs`: real source CLI + production host, typed/null/input/output/cancellation cases | `tests/native_breadth/mod.rs`: resource export/state resolution/release, owner/unknown/retired zero-entry refusal, failed-output provisional cleanup, disposer failure, shutdown cleanup; real policy reverse reply + typed diagnostics/invalid recovery; artifact success/signature refusal; before-prompt hook | **Parent production-host run: 9/9 passed, including four new breadth tests** (`artifacts/takeover/native-ts-r3.receipt.json`). No native bulk, private session hook, retained service, full lifecycle race matrix, audio/provider/TUI or installed-release claim |
| C | `sdk/rust/host-check/src/lib.rs`: actual C executable negotiation/input validation/text/cancellation/shutdown | None | No structured output/resources/hooks/media ABI; raw FFI tests are separate evidence |
| C++ | Same native host test invokes actual C++ executable | None | Same ABI limits; wrapper exception tests do not prove broader service reachability |

TS Node subprocess/hostile-input tests (`sdk/typescript/tests/process_*.mjs`)
are useful additional boundary tests but are **not** production-host acceptance.
The new native fixture is an SDK author module, not a Rust host substitute or
handwritten JSON-RPC peer. It uses production discovery, the SDK-generated
launcher, owner contexts, host release status, no restart supervision, per-child
PID evidence and acknowledged shutdown. Media success proves host artifact/result
admission; the public direct-call DTO does not expose decoded media bytes, so this
is not a provider media projection assertion.

## Observed source verification

Later shared-lock stable-source runs: `full-python-r2` passed 43 production-host
cases; `full-rust-r2` passed 49, including actual C/C++ executables; `full-ts-r2`
passed all 9. Receipts are under `artifacts/takeover/`. Python units ran 199 tests
with 1 skip (`python-unit-r4.receipt.json`). These counts do not fill the authoring
API gaps above.

Parent qualification under the shared single-build lock passed all nine native
TS tests: `artifacts/takeover/native-ts-r3.receipt.json` (stable source snapshot).
The policy test uses a test-owned deny consumer through the production host
response API; it proves reverse correlation/delivery, not product policy rules.
Earlier red receipts are retained: missing policy consumer (timeout), then a
private enum import (compile error), both corrected without extending deadlines.
Node process tests passed 57/57; both TS compiler projects passed with TypeScript
6.0.3. These are source-SDK checks, not installed-release or cross-platform gates.
Reproduce with the same isolated environment and single-build lock:

```console
cargo test --offline --locked --manifest-path sdk/typescript/host-smoke/Cargo.toml --test native_typed -- --nocapture --test-threads=1
```

Missing Node/platform/build prerequisites or test failures are blockers, never
successful skips. Receipts bind their exact source snapshots; later edits require
fresh qualification. No full cross-language parity claim follows.

# TypeScript / JavaScript process authoring

`@skaft-software/octet-extension-sdk` is the dependency-free **0.9.0 source
package** for current extension **API `0.4`**. It is a separate local package,
not a rename of the live generated canonical API `0.3` bindings one directory
above. No npm publication or installed-host release qualification is implied.

Requires **Node >=22.19.0**, the same floor as the Pi adapter. Node runs ordinary
ESM JavaScript (`.mjs`) and erasable TypeScript (`.ts` in a `type: module`
package); no compilation, loader dependency, or raw protocol code is needed.
TypeScript enums, parameter properties, JSX and tsconfig path aliases are not
supported by Node's type stripping. For static checking use TypeScript >=5.0.

## One author file

Install from a reviewed checkout into your extension package:

```console
npm pack /absolute/path/to/octet/sdk/typescript/process --pack-destination . --ignore-scripts
npm install --offline --ignore-scripts ./skaft-software-octet-extension-sdk-0.9.0.tgz
```

Give the package `"type": "module"`. Save `extension.ts` (or the same code without
types as `extension.mjs`):

```ts
import { Extension } from '@skaft-software/octet-extension-sdk';

const extension = new Extension();
extension.tool({
  name: 'greet',
  description: 'Return a local greeting',
  parameters: {
    type: 'object',
    properties: {name: {type: 'string', maxLength: 256}},
    required: ['name'],
    additionalProperties: false,
  },
}, ({name}, context) => {
  context.throwIfCancelled();
  return `Hello, ${name}!`;
});
export default extension;
```

Tool arguments are inferred from the literal schema, including required versus
optional properties. A schema is **mandatory**, never invented from a JavaScript
function's parameter names. For a genuinely argument-free tool, explicitly use
`{type: 'object', properties: {}, additionalProperties: false}`. Optional argument
defaults belong in the handler (`{name = 'world'}`), not implicit wire mutation.

## Typed structured output

Use `typedTool` when callers need a value, not a JSON string hidden in text:

```ts
extension.typedTool({
  name: 'square', description: 'Square a number',
  parameters: {type: 'object', properties: {x: {type: 'number'}}, required: ['x'], additionalProperties: false},
  outputSchema: {type: 'object', properties: {value: {type: 'number'}}, required: ['value'], additionalProperties: false},
}, ({x}) => ({value: x * x}), result => `Square: ${result.value}`);
```

Inputs, handler output and projection arguments are inferred from the literal
schemas. The SDK publishes `output_schema` and validates the returned value before
emitting `structured_content`. The projection is explicit text, limited to 64 KiB;
the structured value is bounded to 256 KiB and portable JSON. Invalid/nonfinite
output produces an error, not successful text containing a broken value.
`ToolError` remains an intentional domain failure and need not contain output.

For lower-level control, `tool` accepts `outputSchema` and a result
`{text, structuredContent, isError?}`. An explicit `null` is preserved; missing
structured content is refused on a successful schema-declared result. No output
schema means structured content is refused rather than silently untyped.

## Generate the local manifest

Inside a direct child directory named `hello-tools`:

```console
./node_modules/.bin/octet-extension manifest extension.ts --name hello-tools --version 0.1.0 --out extension.toml
```

The CLI derives the tool/command catalog from registration and creates the
manifest plus executable `.octet-launcher.sh`. It refuses to overwrite either
existing file. Regenerate both intentionally
after changing contributions, moving the package, or changing the Node path.
The directory basename must match `--name`. The generated manifest targets that script, which execs the original absolute
installed Node, SDK CLI and author source. The host may stage/copy the script
without relocating Node away from installation-relative dynamic libraries.
Startup does not depend on the host workspace cwd. It is **local packaging**, not a
portable distributable bundle; it deliberately omits `requires_octet`.
Distributable bundles additionally need an exact host-version pin and the
existing bundle validation. The generator executes module registration code;
review it first. It does not run handlers, discover automatically, install,
enable, or grant trust.

Capabilities default to `filesystem = "none"`, `process = false`, `network =
false`. Only when genuinely needed, add `--filesystem workspace|unrestricted`,
`--process`, or `--network` during generation. These are host consent metadata,
**not an OS sandbox**. No provider, credentials, global install or network is
needed for the example.

After reviewing, explicitly enable with a source-built host:

```console
octet --extension-dir /absolute/path/to/my-extensions --enable-extension hello-tools
```

Check `/extensions status`, then invoke the declared tool. Discovery never runs
code and never enables extensions. Full access supplies host authority to an
enabled selected extension without persisting a grant. Under safe/controlled
policies an explicit source-bound host-authority grant is required; safe mode
is not an OS sandbox. See [the host guide](../../../docs/extensions.md).

The runnable [text statistics example](../../../examples/extensions/typescript-hello/README.md)
is one author source file plus a local package and generated manifest.

## Handlers and explicit limitations

- Return a string, or `{text: string, isError?: boolean}`. Text is lowered into
  the host's typed content parts. Throw `ToolError('message')` only for an
  intentional model-visible failure; other exceptions return a generic internal
  error without copying the exception to the wire or diagnostic log.
- Every handler receives `context.signal` (AbortSignal),
  `context.throwIfCancelled()` and `await context.sleep(ms)`. Yield and check
  cancellation **between effects**; an effect already performed is not rolled
  back and ambiguous unsafe work must not be replayed. Pass the signal to
  cancellable Node APIs when appropriate. Synchronous CPU work cannot receive a
  cancellation notification until it yields; the host supervises the process.
- `context.workspace`, `context.host` and optional `context.resource_owner` are
  host metadata, separate from model arguments. Session-owned state must use the
  complete owner/instance/generation triple; it is not reverse-service authority.
- `if (context.supportsProgress) await context.progress('Counting',
  {current: 1, total: 2, unit: 'items'})` emits negotiated ephemeral status.
  Unoffered progress throws `UnsupportedFeatureError`; stale contexts cannot
  emit. Progress is not tool output and not persisted in the conversation.
- `extension.command({name: 'hello', description: 'Show hello'},
  (arguments, context) => 'Hello')` supplies a fixed slash-command catalog.
  Arguments are strings; return a string. Commands use the same cancellation
  and progress context as tools.
- `extension.onShutdown(async ({reason}) => { /* local cleanup */ })` registers
  one bounded cleanup callback. Reasons are `shutdown` or `transport_lost`.
  It is not a model hook and does not own host cleanup.
- Named hooks use `extension.hook(name, (payload, context) => result)` with
  hook-specific result validation. Session observations are continue-only;
  private append grants advance only through host commit receipts.
- `resourceType(name, dispose)` supplies nominal schemas for fixed object fields.
  `context.exportResource`, `context.resource` and `context.releaseResource`
  expose registration, admitted native identity and retirement. Resource tools
  publish operation descriptors and serialize handlers. Arrays/root resource
  positions are rejected; copied schema JSON is not a resource declaration.
- Results may include validated `diagnostics` (metadata plus bounded text) and
  image/audio `media` parts. `context.publishArtifact(bytes, mimeType)` publishes
  at most 256 KiB inline; signature/owner checks remain host-owned.
- `context.request(method, params)` is a bounded reverse-service allowlist, not
  arbitrary RPC or retained authority. The SDK owns correlation and cancellation;
  callers cannot supply authority fields. Declare needed service features with
  `new Extension({features: [...]})`.
- Bulk supports `blobSchema`, `validateBlob`, negotiated transport metadata and
  low-level write/commit/read/release requests. **No secure local-file I/O helper
  is implemented**; descriptor reachability is not bulk authoring parity.
- Dynamic tools, flags, remote UI, lifecycle subscriptions, retained-owner and
  session-control/child services are **not implemented by this package**. See
  [the source parity inventory](../../conformance/sdk-parity.md) for separate
  implementation, reachability and native-evidence axes.

Keep module top-level code to imports and registration. Export the Extension;
do **not** call `run()` in a CLI-loaded module. The launcher owns the process
loop. For a standalone `.mjs` entrypoint, `extension.run()` is available instead.
`run()` owns process stdio and exits at its terminal boundary. `console.*`
logging is redirected to bounded stderr; direct `process.stdout.write` is
rejected, including during CLI module loading. Do not bypass that writer or log
secrets. Child processes/native code still run with OS authority; this is stdout
discipline, not isolation.

## Runtime bounds

The wire is current API `0.4` feature-negotiated JSONL, **not canonical API `0.3`**.
It selects offered `request_progress`, required `request_cancellation` and
`content_parts`, plus declared hook/resource and explicitly requested service
features. Missing required author features fail initialization; API versions and
the exact manifest catalogs must match. Reverse calls have a 30-second deadline,
32 concurrent children and 65,536 non-reused IDs per process generation.

- UTF-8 frames are at most **1 MiB including LF** in either direction, matching
  the stateful Rust host reader. Invalid UTF-8 or overlong frames terminates the
  generation. Malformed JSON/envelopes/params return standard bounded errors.
  Partial final input at EOF is discarded, never executed.
- One writer serializes complete frames; at most 128 await it. Exhaustion is a
  terminal transport failure rather than an unbounded output queue.
- Initialization waits at most 30 seconds. Handler concurrency is the lesser of
  the host offer and `maxConcurrentRequests` (default 4; 1..64); at most 64
  requests are active or queued. Admission overflow returns a server error.
- Cancellation settles the original ID at most once with `-32800` if it wins;
  a completed result can win the race. Queued cancelled work never runs. A
  handler that does not settle within the host's 2-second grace terminates the
  generation, without claiming rollback or a successful cancellation response.
- Shutdown stops admission and cancels remaining work. Drain plus cleanup is
  capped at 1 second; acknowledgement/flush/exit is capped at 1.3 seconds, below
  the host's 1.4-second coordinated-signal cap. A stuck drain/hook exits nonzero.
  EOF performs bounded transport-loss cleanup, without a shutdown acknowledgement.
- The authoring subset additionally limits plain JSON to depth 32 / 16,384 nodes
  and portable safe integers. Host-issued request IDs must be safe unsigned
  numeric IDs. Catalogs are capped at 256 each; names follow the host's 64-byte
  ASCII identifier rule. Status messages are at most 8 KiB; console diagnostics
  at most 4 KiB per call / 64 KiB per loading or running stage.

Supported schema keywords are `type`, `properties`, `required`, boolean
`additionalProperties`, `items`, scalar `enum`, `description`, `title`,
`minimum`, `maximum`, `minLength`, `maxLength`, `minItems` and `maxItems`.
Every node needs a type; arrays need `items`. Unknown keywords fail registration
rather than weakening validation. This is a deliberately small validator, not a
full JSON Schema engine. No new npm dependencies are needed.

Launcher generation is currently Unix-only and refuses Windows explicitly.
Actual native launch evidence is macOS (including Homebrew Node 22 with
installation-relative dylibs); Windows and Linux native launch are not claimed
qualified. Use the reviewed tarball installation, not a mutable `file:` checkout
symlink, when preserving dependency closure.

## Source verification

From the repository root, with Node >=22.19:

```console
npm --prefix sdk/typescript test
npm --prefix sdk/typescript run test:package
npm --prefix sdk/typescript run test:process
# With an available TypeScript >=5 compiler:
tsc -p sdk/typescript/tsconfig.json
tsc -p sdk/typescript/tests/tsconfig.process.json
python3 scripts/generate-extension-api-v03.py --check
```

The process tests use real children for framing, exact initialize, tools,
commands, progress, cancellation, shutdown, hostile inputs and an offline
packed-SDK TypeScript consumer. Type checking the example requires its local
package installation. A provider-free [Rust host smoke](../host-smoke/README.md)
additionally tests the generated manifest against actual discovery, explicit
enablement, negotiation, tool dispatch, cancellation and shutdown.

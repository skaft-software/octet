# TypeScript / JavaScript extension SDKs

The **0.8.2 source distributions** have two separate packages/wires. No npm
publication or registry availability is asserted.

## Current API `0.4` process authoring

Use the dependency-free local package
[`@skaft-software/octet-extension-sdk`](process/README.md) under `process/`.
Node >=22.19.0 runs a single ESM JavaScript or erasable TypeScript author file;
`Extension` handles framing, exact negotiation, tools, commands, cancellation,
progress and bounded shutdown. The CLI generates the local manifest from
registration. Hooks and unimplemented services fail explicitly, never as no-ops.

Try the [runnable text statistics example](../../examples/extensions/typescript-hello/README.md):

```console
npm run pack:sdk --prefix examples/extensions/typescript-hello
npm install --offline --ignore-scripts --no-package-lock --prefix examples/extensions/typescript-hello
npm run manifest --prefix examples/extensions/typescript-hello
octet --extension-dir ./examples/extensions --enable-extension typescript-hello
```

Manifest generation executes registration code; review the source first.
Discovery does not run or enable code. This local source recipe is not a
portable bundle or publication claim. [Provider-free process and Rust host
checks](host-smoke/README.md) cover more than loading declarations.

## Retained canonical API `0.3` bindings

The parent package remains **`@skaft-software/octet-extension-api-v03`**:
schema-generated ESM runtime and TypeScript declarations for the supported,
distinct canonical API `0.3` contract. It is not relabeled as API `0.4` and is
not a general process runtime. Existing generated conformance and offline
package tests remain live.

```js
import { hostOffer, selectRequired, negotiate } from '@skaft-software/octet-extension-api-v03';

const offer = hostOffer(1_048_576, 4);
const contract = negotiate(offer, selectRequired(offer));
```

This is a contract-binding example, **not a complete runnable extension**. It
does not demonstrate a manifest, stdio process loop, dispatch, cancellation or
shutdown. The retained dependency-free [API `0.3` minimal process](../../examples/extensions/api-v03-minimal/README.md)
keeps its exact `=0.8.0` host pin, API `0.3`, and extension version `0.1.0`.

The source distribution version `0.8.2` is independent of extension API `0.3`.
Native octet publication does not publish this package to npm. Generated
runtime/declarations have no registry dependencies. Generated files come from
`protocol/extension-api-v0.3.schema.json`; **do not edit them directly**.

See [current extension authoring](../../docs/extensions.md), the
[canonical generated reference](../../docs/extensions/API-0.4-REFERENCE.md) and
[feature-negotiated wire](../../docs/extensions/PROTOCOL-REFERENCE.md) for exact
version-specific fields, bounds and host availability.

# @skaft-software/octet-extension-api-v03

Schema-generated ESM runtime and TypeScript declarations for the canonical octet
extension API `0.3` contract. This example negotiates a contract locally:

```js
import { hostOffer, selectRequired, negotiate } from '@skaft-software/octet-extension-api-v03';

const offer = hostOffer(1_048_576, 4);
const contract = negotiate(offer, selectRequired(offer));
```

This is a contract-binding example, **not a complete runnable extension**. It
does not demonstrate a manifest, stdio process loop, tool dispatch, cancellation,
or shutdown. Do not infer a complete extension runtime from generated bindings.

The source package distribution version is `0.7.3`, independent of extension
API `0.3`. The package name does not assert npm publication or registry
availability; native octet publication does not publish this SDK to npm.
Generated declarations/runtime have no registry dependencies.

See [current extension authoring](../../docs/extensions.md) and the
[generated API reference](../../docs/extensions/API-0.3-REFERENCE.md) for exact
fields, bounds, nullable/optional distinctions, errors, and available methods.
Generated files come from `protocol/extension-api-v0.3.schema.json`; do not edit
them directly.

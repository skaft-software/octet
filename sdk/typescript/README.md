# @skaft-software/octet-extension-api-v03

Schema-generated ESM runtime and TypeScript declarations for the canonical octet extension API 0.3 contract.

```js
import { hostOffer, selectRequired, negotiate } from '@skaft-software/octet-extension-api-v03';

const offer = hostOffer(1_048_576, 4);
const contract = negotiate(offer, selectRequired(offer));
```

The source package distribution version is `0.7.0`; extension API `0.3` remains
independent. This name does not assert npm publication or registry availability.

Generated files are regenerated from `protocol/extension-api-v0.3.schema.json`; do not edit them directly.

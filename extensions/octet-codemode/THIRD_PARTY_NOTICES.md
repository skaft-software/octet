# Third-party notices

The octet adapter is MIT licensed: [LICENSE](LICENSE).

## Pi codemode 1.0.0

- Package: `@earendil-works/pi-codemode@1.0.0`
- Upstream: https://github.com/earendil-works/pi
- Published Git head: `a13d35a742c6ef8462812a28fbe1d8c8b7431c32`
- Copyright (c) 2025 Mario Zechner
- License: MIT; complete text: [vendor/pi-codemode/LICENSE](vendor/pi-codemode/LICENSE)

The Rust executable embeds the published prelude, identifier normalization and
declaration-renderer sources at compile time: `dist/runtime/prelude-source.js`,
`dist/identifier.js`, `dist/declarations.js` and the `dist/source.js` grammar
source that the unit test pins. No retained runtime source is patched. The
Node-only loaders, declarations and source maps of the same release are not
shipped. The npm archive omits its license; the exact published revision's MIT
license is retained both alongside the code and in `vendor/sources/pi-LICENSE`.

## QuickJS WASI 3.6.2

- Package: `quickjs-wasi@3.6.2`
- Upstream: https://github.com/vercel-labs/quickjs-wasi
- Copyright (c) 2026 Vercel, Inc.
- License: MIT; complete text: [vendor/quickjs-wasi/LICENSE](vendor/quickjs-wasi/LICENSE)

The published core WASM is embedded in the Rust executable and the published
JavaScript loader is not shipped or used (the Rust embedding links the reactor's
exports directly). Optional native `.so` modules are not extracted or used.
Source archives, registry URLs, SHA512 integrity and SHA256 digests are retained
in `vendor/sources/`, `vendor/PROVENANCE.json`, and `vendor/SHA256SUMS`.
`vendor/regenerate.py --check` verifies every retained file offline.

## Embedded engine and runtime components

The wrapper's MIT notice does not replace its embedded components' notices.
Supplemental notices omitted from npm are retained under
[`vendor/quickjs-wasi/licenses/`](vendor/quickjs-wasi/licenses/) and reproduced
from hash-pinned copies in `vendor/sources/`:

- **QuickJS-NG**: MIT, including Fabrice Bellard, Charlie Gordon, Ben Noordhuis
  and Saúl Ibarra Corretgé's copyrights. Source: submodule commit
  `6d46d07d04041b40f4f49eaa7fdebe44c314c699` from the published wrapper tag
  `quickjs-wasi@3.6.2` (`5a7a0eeda87c99542f8cf3095b6d61ecfa755977`).
- **WASI libc and LLVM runtime**: MIT, Apache-2.0 with LLVM exceptions, and
  retained BSD/musl notices. Pins follow the wrapper's wasi-sdk-32 build inputs.
  The libc umbrella notice also records its public-domain/CC0 dlmalloc component.
- **Optional modules inside the original source archive only**: Ada URL 3.4.3
  uses its MIT alternative; Mbed TLS uses its Apache-2.0 alternative. Their
  notices are retained even though those modules are never extracted or loaded.

Every supplemental notice's upstream URL and SHA256 digest is recorded in
`vendor/PROVENANCE.json`; no runtime code is patched to add these notices.

# Pi release manifests

One JSON file per supported Pi release (`<version>.json`). Together they are the
adapter's only description of a Pi release: `lib/pi-modules.mjs`,
`lib/pi-manifests.mjs`, `lib/installed-pi.mjs` and `shims/local-pi.mjs` read
them, and nothing else names a Pi version or a version range. Adding 1.6 is
adding `1.6.0.json`; the supported range, the module table, the aliases and the
inert-helper routes all follow.

A manifest is deliberately small. It is **not** a copy of Pi's export list.

## Schema (schema 1)

```jsonc
{
  "schema": 1,
  "pi": "1.5.0",                    // must equal the file name
  "reference": {                     // where the release was reviewed from
    "repository": "earendil-works/pi",
    "tag": "v1.5.0",
    "commit": "9c9ce8547…",
    "distribution": "reviewed managed Pi npm distribution"
  },
  "scopes": ["@earendil-works", "@mariozechner"],   // Pi resolves both
  "typebox": {                       // the adapter's pinned TypeBox
    "package": "typebox",
    "spellings": ["typebox", "@sinclair/typebox"]
  },
  "routeBase": "dist",               // symbol routes are relative to <pkg>/dist
  "routes": {                        // specifier Pi's loader resolves
    "pi-ai": {
      "package": "pi-ai",            // installed package
      "export": "./compat",          // package export Pi resolves it to
      "shim": "ai",                  // octet shim for the emulated path (optional)
      "view": {                      // the module Pi 1.5 gives extensions (optional)
        "package": "pi-coding-agent",
        "module": "dist/core/extensions/pi-ai-extension-view.js"
      }
    }
  },
  "symbols": {                       // classification table, per shim package
    "ai": {
      "contentText":        { "surface": "inert", "source": "shim" },
      "parseJsonWithRepair":{ "surface": "inert", "route": "utils/json-parse.js" },
      "validateToolArguments": { "surface": "inert", "route": "utils/validation.js", "fallback": "shim" },
      "reduceAssistantMessageFrames": { "surface": "inert", "route": null, "removed": "1.5.0", "note": "…" },
      "createModels":       { "surface": "authority" }
    }
  },
  "delta": { … }                     // what moved relative to the previous manifest
}
```

Symbol entries:

- `surface: "inert"` — a pure helper, statics or component. `route` names the
  deep module inside the installed package that serves it; `fallback: "shim"`
  means the shim's own adapted implementation answers when no managed install
  does (validation, for instance). `route: null` means the release does not ship
  the symbol: invoking it fails loudly naming the symbol and the version.
- `surface: "inert", source: "shim"` — the shim's own implementation, no
  installed package needed.
- `surface: "authority"` — agent loops, sessions, tool execution, providers,
  credential stores, terminal ownership. Octet keeps these host-owned and the
  existing refusal path applies unchanged.
- Any other symbol a manifest does not describe fails by name
  (`unsupported_feature`), never as `undefined`.

## Resolution rule

`lib/pi-manifests.mjs`:

1. an exact manifest for the installed version wins;
2. a version newer than the highest manifest reuses the nearest older manifest
   **within the same major**, and says so once on stderr
   (`[pi-compat manifest] installed Pi 1.6.0 has no manifest; using 1.5.0 routes
   and classifications unverified`) — an untested release is diagnosed, not
   refused;
3. a version below the oldest manifest, in another major series, or not
   `x.y.z` (no leading zeros) resolves to nothing and is refused with the
   supported range and shipped manifests named.

`PI_SUPPORTED_RANGE` is computed from the shipped manifests
(`>=1.0.2 <2.0.0` today) and never hand-written.

## Routes are deep modules only

Every `route` and `view.module` names a specific file. No inert helper is
imported through a package root, so resolving a helper can never start Pi's
process-wide handlers, filesystem patches, provider runtimes or extension
runner. `test/pi-manifests.test.mjs` enforces that, checks that every shim
export is classified, and — when the matching managed install is present —
checks that every route resolves to a file that actually exports its symbol.

## Regenerating a manifest

Manifests are reviewed data, not generated build output. To add a release:

1. install it under `~/.pi/agent/install/releases/<version>` (or point
   `OCTET_PI_AGENT_DIR` at a fixture with the same layout);
2. copy the previous manifest and set `pi`/`reference`;
3. re-check every `symbols.*.*.route` against `<release>/node_modules/@earendil-works/<pkg>/dist/`
   — a moved helper is one route edit; a removed one becomes `route: null`;
4. record what moved in `delta`;
5. add the release's `shims/pi-<version>/UPSTREAM.json` hashes;
6. run `node --test test/pi-manifests.test.mjs`.

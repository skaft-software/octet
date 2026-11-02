# Pi-compat: native ESM loading for already-ESM factories

Status: implemented on `perf/pi-native-esm`. Scope: `extensions/octet-pi-compat` only.

## Problem

Cold mirror prepare is floor-bound by jiti's one-time babel transform of bundles that are
already plain ESM JavaScript (`pi-lens/dist/index.js` 6 MB ~8 s; pi-subagents, pi-web-access,
pi-advisor-flow, pi-permission-system 1.5-2 s each). Warm runs still read the cached ~MB
transform output and `runInThisContext` it.

## Current behavior (why jiti transforms them)

- `lib/runtime.mjs:384` creates `createJiti(import.meta.url, { alias, moduleCache: true, tryNative: false, ...cache })`;
  `:453` loads each factory with `jiti.import(entry, { default: true })`.
- jiti 2.7.0 `eval_evalModule` (`node_modules/jiti/dist/jiti.cjs`): a `.mjs`, or a `.js` whose nearest
  `package.json` has `"type": "module"`, imported asynchronously, is **first imported natively**
  (`nativeImportOrRequire`); only if that rejects does it retry with `forceTranspile`
  (babel). `tryNative: false` does not change this per-file attempt.
- That native attempt fails for these bundles solely because they statically import Pi package
  names (`@earendil-works/*`, `@mariozechner/*`, `typebox`): Node cannot resolve them
  (jiti's `alias` only applies inside jiti's own resolver). Link fails with `ERR_MODULE_NOT_FOUND`
  *before any evaluation*, jiti falls back, and babel transforms the whole file (measured with
  `JITI_DEBUG`: `[native][import] ... Native import error ... [fallback] ... [transpile][esm] (9335ms)`).
  Children are then reached via `jitiImport`, which resolves the alias to the shim file and
  imports that *natively* (`shims/*.mjs` are already native modules today).
- So the transform is paid only to rewrite alias specifiers (and to give the file jiti's
  CJS-style wrapper variables `require`, `module`, `exports`, `__filename`, `__dirname`).
- Integrity today: `reviewedSourceHashes` (`lib/runtime.mjs:142`) hashes the entry plus its
  relative-import closure (`IMPORT_SPECIFIERS`, `:91`: `./`/`../` specifiers only; symlink, size and
  read-race checks). `load()` (`:420-428`) compares it to `config.entrypoint_sources[entry]` before
  any import. Bare-specifier dependencies (`node_modules`) are **not** hashed; they execute unreviewed,
  natively when ESM or CJS-without-ESM-syntax, transformed otherwise. The import itself reads the
  file again at import time (same window for jiti and native).

## Proposal

Teach Node's native ESM loader the alias table, for the reviewed closure only, and let the existing
native attempt succeed. `lib/native-esm.mjs`:

1. `module.registerHooks` (sync, Node >= 22.15; prerequisite is 22.19, `lib/runtime-prerequisites.mjs`)
   installs one `resolve` and one `load` hook, once per process. Absent API or `OCTET_PI_NATIVE_ESM=0`
   disables everything (pure jiti).
2. Scope. A hook acts only when the importing module (`parentURL`) is a file in a registered
   factory's reviewed closure (`config.entrypoint_sources[entry]` keys), and the factory's scope is
   not `disabled`. Everything else (adapter, shims, node_modules deps, other factories' non-closure files)
   falls through untouched.
3. `resolve`: an exact alias key (the same table `emulatedPiAliases()` + `typeboxAliases()` that
   jiti receives) resolves to the shim file URL. That is the same file URL jiti's `jitiImport` hands to
   `import()`, so the shim is **one ES module instance** in Node's loader cache.
4. `load`: serves the closure file with the sha256 of the bytes checked against the reviewed hash
   (new, strictly stronger than jiti reading the file unchecked), and prepends one line (no newline,
   so line numbers stay) defining only the jiti wrapper variables the file references and does not
   declare itself (`require`/`module`/`exports`/`__filename`/`__dirname`), so `typeof __dirname`
   style code sees what it saw under jiti.
5. Eligibility is decided before any import (`analyze`), else the factory goes the jiti route:
   every closure file is `.js`/`.mjs` ESM (`.js` only in a `"type": "module"` package), no hashbang,
   no `import.meta.*` other than `url|dirname|filename|resolve` (jiti special-cases others), runtime mode
   is `shims` (the installed-Pi route keeps jiti), and mirror review data is present.
6. Probing. The entry is imported natively once inside a "probing" window with these extra resolve
   checks, each of which throws a private fallback error (pre-evaluation, since resolution/link errors
   precede evaluation): an alias to `unavailable.mjs`, a non-exact subpath of an alias, a closure file
   importing a file outside the reviewed closure that is not under `node_modules`, a target that is not
   native ESM (CJS, JSON, TS, native addon: where jiti's interop proxy semantics apply), non-`file:`/`node:` URLs.
   Any rejection of the probe (including a link `SyntaxError` for a missing named export and
   evaluation errors) disables that scope and falls back to today's `jiti.import`, which then re-runs
   exactly as before (jiti itself already re-runs on a native evaluation error). Success marks the scope
   `live`: later dynamic imports from the closure still get alias resolution but are never rejected.
7. After a successful probe, `jiti.import(entry, { default: true })` is still what returns the factory:
   jiti's native attempt hits the already-evaluated module (same instance) and applies its default/interop
   wrapper, so the returned function is exactly what it was. Registration order and `fn(pi)` sequencing are
   unchanged (the loop at `lib/runtime.mjs:~445` is untouched).
8. Warm-up: `configure.mjs` no longer queues babel warm-up for closures that are native-eligible.

## Identity argument

Shims are native modules keyed by file URL. Under jiti, `jitiImport(alias)` resolves to the shim path and
calls `import(path)`; under the hook, `import '@earendil-works/pi-coding-agent'` resolves to
`pathToFileURL(sameShimPath)`. Node's ESM cache is keyed by URL, so a TS factory (jiti) and a JS factory
(native) get the same namespace object (jiti wraps it in an interop `Proxy` for its importers; the evaluated
module and all state are shared). TypeBox aliases resolve to the same file `pick(exports)` chooses, equal to
what the shims import natively. Test: `test/native-esm.test.mjs`.

## Integrity argument

The file set that executes is the same as before: the closure files (already hashed pre-import) plus
node_modules deps (unreviewed both ways). Native loads execute only files reached by specifiers that Node
resolves; during probing any closure-to-non-closure, non-node_modules resolution aborts to jiti, so native
loading can never execute a source file the review did not list that jiti would not also have executed.
The hash check in `load` means native execution is of exactly the reviewed bytes (closing the usual
read-to-import window for those files); a mismatch is a hard refusal, not a fallback.

## Fallback

Per factory, on any doubt: ineligible analysis, hook API missing, probe failure of any kind. The fallback
is the previous code path, byte-for-byte. Residual differences accepted and documented: (a) CJS static
imports from closure files send the whole factory to jiti (interop semantics), so only factories whose
closure statically imports ESM/`node:` targets benefit; (b) dynamic `import()` from native closure files
no longer gets jiti's TS/JSX transform for the target (Node's own rules apply; literal relative targets are
guaranteed JS by the eligibility check, non-literal targets are node_modules/URL imports in practice, e.g.
pi-lens loading `web-tree-sitter`/`@ast-grep/napi`); (c) free `require` in a native file is
`createRequire(import.meta.url)`, not jiti's resolver (so no TS/alias-aware sync `require`); `exports`/`module`
are inert objects. Kill switch: `OCTET_PI_NATIVE_ESM=0`.

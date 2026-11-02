import { createRequire } from 'node:module';
import { readFileSync, statSync } from 'node:fs';
import { dirname, isAbsolute, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createJiti as stockCreateJiti } from 'jiti';
import { readPersisted, writePersisted } from './startup-cache.mjs';

// Persistent module-resolution map for jiti.
//
// jiti 2.7.0 re-derives every resolution (stat storms, thrown NodeErrors) in
// each process and exposes no resolution hook. The bundle is therefore loaded
// from its own pinned file with one exact textual anchor rewritten so that
// `jitiResolve` first consults `hook(ctx, specifier, options, original)`. If the
// installed jiti is not exactly the pinned version, or the anchor is absent,
// the stock `createJiti` is used unchanged.
//
// An entry maps (alias table, parent, specifier, conditions, ...) to the
// resolved file and names the stat fingerprints it depends on: the resolved
// file and its directory, the parent directory, every `node_modules` directory
// on the way up from the parent, and the nearest package.json above the result
// (the one that governs `exports`/`main`). Every one must be byte-for-byte the
// same fingerprint (dev, ino, size, mtime ns, ctime ns) or the entry is dropped
// and jiti resolves for real. Only successful absolute-path resolutions are
// stored; failures and `try` misses are never cached. The cache directory is
// the same owner-only directory as the transform and V8 compile caches and is
// trusted to the same degree.
const SCHEMA = 'octet.pi-jiti-resolve/v1';
const FILE = 'jiti-resolve-v1.json';
const PINNED = '2.7.0';
const ANCHOR = 'function jitiResolve(e,t,i){let n,a;';
const MAX_ENTRIES = 60000;

const bigStat = path => { try { const s = statSync(path, { bigint: true }); return `${s.dev}:${s.ino}:${s.size}:${s.mtimeNs}:${s.ctimeNs}`; } catch { return null; } };

class ResolveCache {
  constructor() {
    this.entries = new Map(); // key -> { r, d: [pathId, printId, ...] } (fingerprints are per entry)
    this.paths = [];           // interned paths
    this.prints = [];          // interned fingerprints (null = absent)
    this.ids = new Map();      // path -> id
    this.printIds = new Map(); // fingerprint -> id
    this.current = new Map();  // path -> fingerprint seen this process (memo)
    this.verified = new Set(); // keys already validated this process
    this.dirty = false;
    const stored = readPersisted(FILE, SCHEMA, 64 * 1024 * 1024);
    if (stored && Array.isArray(stored.paths) && Array.isArray(stored.prints) && stored.entries && typeof stored.entries === 'object') {
      this.paths = stored.paths; this.prints = stored.prints;
      stored.paths.forEach((path, id) => this.ids.set(path, id));
      stored.prints.forEach((print, id) => this.printIds.set(print, id));
      for (const [key, entry] of Object.entries(stored.entries)) {
        if (typeof entry?.r === 'string' && Array.isArray(entry.d) && entry.d.length % 2 === 0
            && entry.d.every((id, index) => Number.isInteger(id) && id >= 0 && id < (index % 2 ? this.prints : this.paths).length)) this.entries.set(key, entry);
      }
    }
    this.aliasKeys = new WeakMap();
  }
  now(path) {
    let print = this.current.get(path);
    if (print === undefined) { print = bigStat(path); this.current.set(path, print); }
    return print;
  }
  valid(entry) {
    if (!isAbsolute(entry.r)) return false;
    for (let i = 0; i < entry.d.length; i += 2) if (this.now(this.paths[entry.d[i]]) !== this.prints[entry.d[i + 1]]) return false;
    return true;
  }
  aliasKey(alias) {
    if (!alias || typeof alias !== 'object') return '';
    let key = this.aliasKeys.get(alias);
    if (key === undefined) { key = JSON.stringify(alias); this.aliasKeys.set(alias, key); }
    return key;
  }
  key(ctx, specifier, options) {
    const parent = options?.parentURL ?? ctx.url;
    return JSON.stringify([this.aliasKey(ctx.alias), ctx.opts?.extensions, String(parent?.href ?? parent), specifier,
      !!options?.async, options?.conditions ?? null, options?.paths ?? null, !!options?.try, ctx.filename, ctx.parentModule?.filename ?? null]);
  }
  dependencies(parentFile, resolved) {
    const out = new Set([resolved, dirname(resolved)]);
    if (parentFile) {
      for (let dir = dirname(parentFile), last; dir !== last; last = dir, dir = dirname(dir)) { out.add(dir); out.add(join(dir, 'node_modules')); }
    }
    for (let dir = dirname(resolved), last; dir !== last; last = dir, dir = dirname(dir)) {
      out.add(join(dir, 'package.json'));
      try { statSync(join(dir, 'package.json')); break; } catch { /* keep walking up */ }
    }
    return out;
  }
  intern(ids, table, value) {
    let id = ids.get(value);
    if (id === undefined) { id = table.length; table.push(value); ids.set(value, id); }
    return id;
  }
  hook = (ctx, specifier, options, original) => {
    if (typeof specifier !== 'string' || specifier.startsWith('node:') || specifier.startsWith('data:')) return original(ctx, specifier, options);
    let key;
    try { key = this.key(ctx, specifier, options); } catch { return original(ctx, specifier, options); }
    const hit = this.entries.get(key);
    if (hit) {
      if (this.verified.has(key) || this.valid(hit)) { this.verified.add(key); return hit.r; }
      this.entries.delete(key);
    }
    const result = original(ctx, specifier, options);
    if (typeof result === 'string' && isAbsolute(result) && this.entries.size < MAX_ENTRIES) {
      try {
        const parent = options?.parentURL ?? ctx.url, parentFile = parent ? fileURLToPath(parent.href ?? parent) : undefined;
        const ids = [];
        for (const path of this.dependencies(parentFile, result)) {
          ids.push(this.intern(this.ids, this.paths, path), this.intern(this.printIds, this.prints, this.now(path)));
        }
        this.entries.set(key, { r: result, d: ids });
        this.verified.add(key);
        this.dirty = true;
      } catch { /* an uncacheable resolution is simply resolved again next time */ }
    }
    return result;
  };
  flush() {
    if (!this.dirty) return;
    this.dirty = false;
    // Persist only entries that validate against the current tree.
    // Rebuild the interned tables from live entries so dead fingerprints do not accumulate.
    const paths = [], prints = [], pathIds = new Map(), printIds = new Map(), entries = {};
    for (const [key, entry] of this.entries) {
      if (!this.verified.has(key) && !this.valid(entry)) continue;
      const d = [];
      for (let i = 0; i < entry.d.length; i += 2) d.push(this.intern(pathIds, paths, this.paths[entry.d[i]]), this.intern(printIds, prints, this.prints[entry.d[i + 1]]));
      entries[key] = { r: entry.r, d };
    }
    writePersisted(FILE, { schema: SCHEMA, jiti: PINNED, node: process.version, paths, prints, entries }, 64 * 1024 * 1024);
  }
}

let cache, patched;
export function resolveCache() {
  if (!cache) { cache = new ResolveCache(); process.once('exit', flushResolveCache); }
  return cache;
}
/// Persists newly learned resolutions. Cheap no-op when nothing changed.
export function flushResolveCache() { try { cache?.flush(); } catch { /* optimization only */ } }

function patchedFactory() {
  if (patched !== undefined) return patched;
  patched = null;
  try {
    const require = createRequire(import.meta.url);
    const manifest = require.resolve('jiti/package.json');
    if (JSON.parse(readFileSync(manifest, 'utf8')).version !== PINNED || process.env.OCTET_PI_JITI_RESOLVE_CACHE === '0') return patched;
    const dist = join(dirname(manifest), 'dist', 'jiti.cjs'), source = readFileSync(dist, 'utf8');
    if (source.split(ANCHOR).length !== 2) return patched;
    const rewritten = source.replace(ANCHOR, 'function jitiResolve(e,t,i){const h=__octetResolveHook;return h?h(e,t,i,jitiResolveOriginal):jitiResolveOriginal(e,t,i)}function jitiResolveOriginal(e,t,i){let n,a;');
    const module = { exports: {} };
    new Function('require', 'module', 'exports', '__filename', '__dirname', '__octetResolveHook', rewritten)(createRequire(dist), module, module.exports, dist, dirname(dist), resolveCache().hook);
    const create = module.exports;
    const distRequire = createRequire(dist);
    let babel;
    const lazyTransform = (...args) => (babel ??= distRequire('./babel.cjs'))(...args);
    const onError = error => { throw error; };
    const nativeImport = id => import(id);
    patched = (id, opts = {}) => create(id, opts.transform ? opts : { ...opts, transform: lazyTransform }, { onError, nativeImport, createRequire });
  } catch { patched = null; }
  return patched;
}

/// `createJiti` with the persistent resolution map when the pinned jiti allows it.
export function createJiti(id, opts = {}) {
  const factory = process.env.OCTET_PI_JITI_RESOLVE_CACHE === '0' ? null : patchedFactory();
  return (factory ?? stockCreateJiti)(id, opts);
}

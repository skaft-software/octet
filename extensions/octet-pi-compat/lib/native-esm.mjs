import * as nodeModule from 'node:module';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, extname, join, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { tracePhase } from './startup-trace.mjs';

// Native ESM loading for already-ESM factories. See docs/design/pi-native-esm.md.
//
// jiti natively imports `.mjs` / `type: module` `.js` files first and only babels
// them when that import rejects. For Pi extension bundles the rejection is just
// the unresolvable Pi package names. A synchronous resolve hook that knows the
// adapter's alias table, scoped to a reviewed factory's source closure, lets the
// native import succeed, so jiti never transforms the file. Anything unusual
// disables the scope and the caller runs the unchanged jiti path.

const FALLBACK = 'OCTET_PI_NATIVE_FALLBACK';
const REVIEW = 'OCTET_PI_NATIVE_REVIEW_CHANGED';
const WRAPPER_NAMES = ['require', 'module', 'exports', '__filename', '__dirname'];
const ESM_SOURCE = /\.m?js$/;
const SPECIAL_META = /\bimport\.meta\.(?!(?:url|dirname|filename|resolve)\b)[\w$]+/;
// A write to the CommonJS export objects. The prelude only lets such names be
// read (feature detection); the module's real exports are its ESM ones, so a
// file that publishes through `module.exports` would load natively with an
// empty namespace instead of falling back to jiti's CommonJS interop.
const CJS_EXPORT_WRITE = /\b(?:module\.exports|exports(?:\.[\w$]+|\[[^\]]*\]))\s*=(?!=)|\bObject\.(?:defineProperty|assign)\(\s*(?:module\.)?exports\b/;
const PI_HEAD = /^(?:@earendil-works\/|@mariozechner\/|typebox|@sinclair\/typebox)/;

function fallback(reason) {
  const error = new Error(`native ESM load declined: ${reason}`);
  error.code = FALLBACK;
  return error;
}

const packageTypes = new Map();
function packageType(file) {
  const walked = [];
  let directory = dirname(file), type;
  for (;;) {
    if (packageTypes.has(directory)) { type = packageTypes.get(directory); break; }
    walked.push(directory);
    try { type = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8')).type ?? 'commonjs'; break; } catch { /* keep walking */ }
    const parent = dirname(directory);
    if (parent === directory) { type = 'commonjs'; break; }
    directory = parent;
  }
  for (const seen of walked) packageTypes.set(seen, type);
  return type;
}

export function nativeEsmAvailable() {
  return process.env.OCTET_PI_NATIVE_ESM !== '0' && !process.versions.bun && typeof nodeModule.registerHooks === 'function';
}

// Pure static gate, run before any import. Returns a reason string when the
// factory must stay on jiti, undefined when it may be probed natively.
export function nativeIneligibleReason(entry, hashes) {
  const files = Object.keys(hashes ?? {});
  if (!files.length || !Object.hasOwn(hashes, entry)) return 'no reviewed closure';
  for (const file of files) {
    const extension = extname(file);
    if (!ESM_SOURCE.test(file)) return `closure file is not JavaScript: ${file}`;
    if (extension === '.js' && packageType(file) !== 'module') return `not a type:module package: ${file}`;
    let text;
    try { text = readFileSync(file, 'utf8'); } catch { return `unreadable: ${file}`; }
    if (text.startsWith('#!')) return `hashbang: ${file}`;
    if (SPECIAL_META.test(text)) return `import.meta extension: ${file}`;
    if (CJS_EXPORT_WRITE.test(text)) return `CommonJS export assignment: ${file}`;
  }
  return undefined;
}

const BARE_STATIC = /^[ \t]*(?:(?:import|export)\b[^'"`\n;]*?\bfrom\s*|import\s*|\}\s*from\s*)['"]([^'".\/][^'"\n]*)['"]/gm;

// Performance hint only, never a correctness gate: true when a bare dependency
// of the closure looks like CJS, which makes the native probe decline and the
// factory run on jiti (so its transform should still be warmed). A wrong guess
// costs a babel warm-up or one cold transform, nothing else.
export function nativeLikelyDeclines(hashes, aliases) {
  const known = new Set(Object.keys(aliases));
  const checked = new Set();
  for (const file of Object.keys(hashes)) {
    let text;
    try { text = readFileSync(file, 'utf8'); } catch { return true; }
    for (const [, specifier] of text.matchAll(BARE_STATIC)) {
      if (specifier.startsWith('node:') || known.has(specifier) || PI_HEAD.test(specifier) || nodeModule.builtinModules.includes(specifier)) continue;
      const key = `${dirname(file)}\0${specifier}`;
      if (checked.has(key)) continue;
      checked.add(key);
      let manifest;
      try { manifest = nodeModule.findPackageJSON?.(specifier, pathToFileURL(file)); } catch { continue; }
      if (!manifest) continue;
      let type;
      try { type = JSON.parse(readFileSync(manifest, 'utf8')).type; } catch { return true; }
      if (type !== 'module') return true;
    }
  }
  return false;
}

export class NativeEsm {
  /** `aliases`: the jiti alias table; `unavailable`: the file Pi modules octet does not emulate map to. */
  constructor(aliases, unavailable) {
    this.aliases = new Map();
    for (const [name, file] of Object.entries(aliases)) this.aliases.set(name, { url: pathToFileURL(file).href, unavailable: file === unavailable });
    this.files = new Map(); // file URL -> { path, hash, scope }
    this.installed = false;
    this.nextId = 0;
  }
  install() {
    if (this.installed) return;
    nodeModule.registerHooks({
      resolve: (specifier, context, nextResolve) => this.resolve(specifier, context, nextResolve),
      load: (url, context, nextLoad) => this.load(url, context, nextLoad),
    });
    this.installed = true;
  }
  active(url) {
    const record = url && this.files.get(url);
    return record && record.scope.state !== 'disabled' ? record : undefined;
  }
  resolve(specifier, context, nextResolve) {
    const parent = this.active(context.parentURL);
    if (!parent) return nextResolve(specifier, context);
    const probing = parent.scope.state === 'probing';
    if (PI_HEAD.test(specifier)) {
      const alias = this.aliases.get(specifier);
      if (alias) {
        if (alias.unavailable && probing) throw fallback(`${specifier} is not emulated`);
        return { url: alias.url, format: 'module', shortCircuit: true };
      }
      if (probing && [...this.aliases.keys()].some(name => specifier.startsWith(`${name}/`))) throw fallback(`unmapped subpath ${specifier}`);
    }
    const result = nextResolve(specifier, context);
    if (!probing || result.url.startsWith('node:')) return result;
    if (!result.url.startsWith('file:')) throw fallback(`non-file target ${result.url}`);
    const target = fileURLToPath(result.url);
    if (this.files.get(result.url)?.scope === parent.scope) return result;
    if (!target.includes(`${sep}node_modules${sep}`) && !this.files.has(result.url)) throw fallback(`unreviewed file ${target}`);
    // Dependencies keep jiti's interop semantics unless they are plain ESM.
    const extension = extname(target);
    if (!(extension === '.mjs' || (extension === '.js' && packageType(target) === 'module'))) throw fallback(`non-ESM target ${target}`);
    return result;
  }
  load(url, context, nextLoad) {
    const record = this.active(url);
    if (!record) return nextLoad(url, context);
    const probing = record.scope.state === 'probing';
    const result = nextLoad(url, context);
    if (result.format !== 'module') { if (probing) throw fallback(`format ${result.format} for ${record.path}`); return result; }
    const bytes = Buffer.from(result.source);
    if (createHash('sha256').update(bytes).digest('hex') !== record.hash) {
      const error = new Error(`reviewed factory source changed: ${record.path}; review and configure again`);
      error.code = REVIEW;
      throw error;
    }
    return { format: 'module', source: this.prelude(bytes.toString('utf8')), shortCircuit: true };
  }
  // jiti evaluates factories inside a function that provides CJS-style names.
  // Define the ones the file uses and does not declare itself, on the first line.
  prelude(source) {
    const parts = [];
    const needs = name => new RegExp(`\\b${name}\\b`).test(source)
      && !new RegExp(`(?:\\b(?:const|let|var|function|class)\\s+|\\bas\\s+|\\bimport\\s+(?:\\*\\s+as\\s+)?)${name}\\b`).test(source);
    if (needs('require')) parts.push('import{createRequire as __octetCreateRequire}from"node:module";const require=__octetCreateRequire(import.meta.url);');
    if (needs('module')) parts.push('const module={exports:{}};');
    if (needs('exports')) parts.push('const exports={};');
    if (needs('__filename')) parts.push('const __filename=import.meta.filename;');
    if (needs('__dirname')) parts.push('const __dirname=import.meta.dirname;');
    return parts.length ? parts.join('') + source : source;
  }
  /** Probes a native import of `entry`. True: evaluated natively and the scope is live. False: use jiti. */
  async attempt(entry, hashes, beforeFallback) {
    const reason = nativeIneligibleReason(entry, hashes);
    if (reason) { tracePhase('adapter.native.ineligible', reason); return false; }
    this.install();
    const scope = { id: this.nextId++, state: 'probing' };
    for (const [path, hash] of Object.entries(hashes)) {
      // A file shared with an earlier factory stays with that scope: a later probe that declines must not strip its alias resolution and digest check.
      const url = pathToFileURL(path).href;
      if (!this.active(url)) this.files.set(url, { path, hash, scope });
    }
    try {
      await import(pathToFileURL(entry).href);
      scope.state = 'live';
      tracePhase('adapter.native.loaded', entry);
      return true;
    } catch (error) {
      scope.state = 'disabled';
      if (error?.code === REVIEW) throw error;
      tracePhase('adapter.native.declined', `${entry} ${String(error?.code ?? error?.name)} ${String(error?.message).slice(0, 600)}`);
      beforeFallback?.(); // a failed evaluation may have modified the reviewed files
      return false;
    }
  }
}

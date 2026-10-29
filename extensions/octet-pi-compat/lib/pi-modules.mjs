// The Pi extension API this adapter emulates, and the module specifiers Pi's own
// extension loader resolves for every extension (Pi 1.4.0,
// packages/coding-agent/src/core/extensions/loader.ts getAliases). Both the
// emulated path and the installed-Pi fallback resolve exactly these names, so
// an extension sees the same module graph it sees in Pi.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

export const PI_VERSION = '1.4.0';
// Installed Pi releases the fallback and `--from-pi` accept. 1.0.2 through
// 1.4.x share the install layout, settings and package-manager APIs, and module
// table; only new export names differ, and unclassified names stay refused.
export const PI_SUPPORTED_RANGE = '>=1.0.2 <1.5.0';
export function piReleaseSupported(version) {
  const match = /^1\.(\d+)\.(\d+)$/.exec(version);
  if (!match) return false;
  const [minor, patch] = [Number(match[1]), Number(match[2])];
  return minor <= 4 && (minor > 0 || patch >= 2);
}
export const PI_PREFIXES = ['@earendil-works', '@mariozechner'];

// Specifier (after the scope) -> the Pi package and export it resolves to, and
// the octet shim that stands in for it on the emulated path (none: the module
// is only available through the installed-Pi fallback). Pi resolves the bare
// pi-ai root to its compat entry, a strict superset of the core entry.
export const PI_MODULES = {
  'pi-coding-agent': { pkg: 'pi-coding-agent', key: 'coding-agent', exportKey: '.', shim: 'coding-agent' },
  'pi-agent-core': { pkg: 'pi-agent-core', key: 'agent-core', exportKey: '.' },
  'pi-tui': { pkg: 'pi-tui', key: 'tui', exportKey: '.', shim: 'tui' },
  'pi-ai': { pkg: 'pi-ai', key: 'ai', exportKey: './compat', shim: 'ai' },
  'pi-ai/compat': { pkg: 'pi-ai', key: 'ai', exportKey: './compat', shim: 'ai' },
  // Pi 1.4 aliases pi-ai/models too. Its names are a subset of compat in every
  // supported release, so it is served by the same guarded namespace.
  'pi-ai/models': { pkg: 'pi-ai', key: 'ai', exportKey: './compat', shim: 'ai' },
  'pi-ai/oauth': { pkg: 'pi-ai', key: 'ai-oauth', exportKey: './oauth' },
  'pi-ai/providers/all': { pkg: 'pi-ai', key: 'ai-providers', exportKey: './providers/all' },
};

// Pi resolves both TypeBox spellings to its single TypeBox 1.x install.
const TYPEBOX_SPELLINGS = ['typebox', '@sinclair/typebox'];

/** Splits a specifier into its Pi module descriptor, or null if it is not one. */
export function piModule(specifier) {
  for (const prefix of PI_PREFIXES) {
    if (!specifier.startsWith(`${prefix}/`)) continue;
    const rest = specifier.slice(prefix.length + 1);
    if (Object.hasOwn(PI_MODULES, rest)) return { specifier, name: rest, ...PI_MODULES[rest] };
    const base = Object.keys(PI_MODULES).find(name => rest.startsWith(`${name.split('/')[0]}/`));
    if (base) return { specifier, name: rest, pkg: PI_MODULES[base].pkg, unmapped: true };
  }
  return null;
}

const shimFile = shim => fileURLToPath(new URL(`../shims/${shim}.mjs`, import.meta.url));
// Imported on the emulated path in place of a Pi module octet does not
// emulate; loading it fails, and the load is retried on the installed Pi.
export const UNAVAILABLE_MODULE = fileURLToPath(new URL('../shims/unavailable.mjs', import.meta.url));

/** Emulated-path aliases for every Pi specifier Pi resolves. */
export function emulatedPiAliases() {
  const aliases = {};
  for (const prefix of PI_PREFIXES) {
    for (const [name, module] of Object.entries(PI_MODULES)) {
      aliases[`${prefix}/${name}`] = module.shim ? shimFile(module.shim) : UNAVAILABLE_MODULE;
    }
  }
  return aliases;
}

const pick = target => typeof target === 'string' ? target
  : target && (pick(target.import) ?? pick(target.default) ?? pick(target.node));

/**
 * TypeBox aliases from the adapter's pinned TypeBox (the version Pi 1.0.2
 * through 1.4.0 ship). Pi aliases the root, `compile` and `value`; the other exported
 * subpaths (`system`, `guard`, ...) are resolved too, because a bare alias
 * would otherwise turn `typebox/system` into a path below the root file.
 */
export function typeboxAliases() {
  const root = new URL('../node_modules/typebox/', import.meta.url);
  const exportsMap = JSON.parse(readFileSync(new URL('package.json', root), 'utf8')).exports ?? {};
  const aliases = {};
  for (const [key, target] of Object.entries(exportsMap)) {
    if (key.includes('*') || key === './package.json') continue;
    const file = pick(target);
    if (typeof file !== 'string') continue;
    const subpath = key === '.' ? '' : key.slice(1);
    for (const spelling of TYPEBOX_SPELLINGS) aliases[`${spelling}${subpath}`] = fileURLToPath(new URL(file, root));
  }
  return aliases;
}

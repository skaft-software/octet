// The Pi extension API this adapter emulates, derived from the per-release
// capability manifests in ../shims/manifests/. Both the emulated path and the
// installed-Pi fallback resolve exactly the specifiers Pi's own extension loader
// resolves, so an extension sees the same module graph it sees in Pi.
//
// Nothing here names a Pi release or a version range. `PI_VERSION` is the
// highest manifest this build ships (what the shims emulate when no installed Pi
// answers a module) and `PI_SUPPORTED_RANGE` is computed from the shipped
// manifests. Adding a release is adding shims/manifests/<version>.json.
import { readFileSync, realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { PI_MANIFEST_VERSIONS, piManifest, piRelease, piReleaseSupported, piSupportedRange } from './pi-manifests.mjs';

export { piRelease, piReleaseSupported, PI_MANIFEST_VERSIONS };

/** Supported-version statement computed from the shipped manifests. */
export const PI_SUPPORTED_RANGE = piSupportedRange();
/** The release the shims emulate when no installed Pi answers a module. */
export const PI_VERSION = PI_MANIFEST_VERSIONS[PI_MANIFEST_VERSIONS.length - 1];
export const PI_MANIFEST = piManifest(PI_VERSION);
export const PI_PREFIXES = PI_MANIFEST.scopes;

// Specifier (after the scope) -> the installed Pi package and export it resolves
// to, and the octet shim that stands in for it on the emulated path (none: the
// module is only available through the installed-Pi fallback).
export const PI_MODULES = Object.fromEntries(Object.entries(PI_MANIFEST.routes).map(([name, route]) => [name, {
  pkg: route.package, exportKey: route.export, ...(route.shim ? { shim: route.shim } : {}),
}]));

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

// A module reached through a symlinked directory (pnpm layouts, a linked
// node_modules) would load a second copy of the same module, so aliases point at
// real paths.
const real = path => { try { return realpathSync(path); } catch { return path; } };
const shimFile = shim => real(fileURLToPath(new URL(`../shims/${shim}.mjs`, import.meta.url)));
// Imported on the emulated path in place of a Pi module octet does not
// emulate; loading it fails, and the load is retried on the installed Pi.
export const UNAVAILABLE_MODULE = real(fileURLToPath(new URL('../shims/unavailable.mjs', import.meta.url)));

/** Emulated-path aliases for every Pi specifier the manifest routes. */
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
 * TypeBox aliases from the adapter's pinned TypeBox. Pi aliases the root,
 * `compile` and `value`; the other exported subpaths (`system`, `guard`, ...)
 * are resolved too, because a bare alias would otherwise turn `typebox/system`
 * into a path below the root file.
 */
export function typeboxAliases() {
  const root = new URL(`../node_modules/${PI_MANIFEST.typebox.package}/`, import.meta.url);
  const exportsMap = JSON.parse(readFileSync(new URL('package.json', root), 'utf8')).exports ?? {};
  const aliases = {};
  for (const [key, target] of Object.entries(exportsMap)) {
    if (key.includes('*') || key === './package.json') continue;
    const file = pick(target);
    if (typeof file !== 'string') continue;
    const subpath = key === '.' ? '' : key.slice(1);
    for (const spelling of PI_MANIFEST.typebox.spellings) aliases[`${spelling}${subpath}`] = real(fileURLToPath(new URL(file, root)));
  }
  return aliases;
}

// Manifest-driven binding of the inert Pi surface the shims expose.
//
// The shims declare *which symbols* they bind, never *where those symbols live*.
// A symbol's deep module comes from the manifest that governs the installed Pi
// release (lib/pi-manifests.mjs), so a Pi release that moves a helper is one
// manifest edit, not a shim rewrite. Only specific files are imported — never a
// package root — so an inert helper can never drag in Pi's process-wide
// handlers, filesystem patches or provider runtimes.
//
// Failure modes are loud and named:
//   - no supported installed Pi -> installed_pi_unavailable (a shim-owned
//     fallback is used instead where the symbol has one);
//   - the manifest gives no route for the symbol (removed in this release) ->
//     unsupported_feature pi-ai.<symbol>, naming the symbol and the version;
//   - the manifest routes the symbol but the installed module does not export it
//     -> pi_compat_helper_absent, naming the symbol, the release and the module.
// A shim binding is never `undefined`.
import { createRequire } from 'node:module';
import { join, relative, isAbsolute } from 'node:path';
import { readFileSync, realpathSync, statSync } from 'node:fs';
import { locateInstalledPi, PACKAGES, INSTALLED_PI_UNAVAILABLE } from '../lib/installed-pi.mjs';
import { piRelease } from '../lib/pi-manifests.mjs';
import { rpcError, unsupported } from '../lib/errors.mjs';

const require = createRequire(import.meta.url);
const configKey = Symbol.for('octet.pi-compat.local-pi-dir');
export function configureLocalPi(directory) { globalThis[configKey] = directory; }

export const HELPER_ABSENT = -32032;
const absent = message => rpcError(HELPER_ABSENT, `pi_compat_helper_absent ${message}`);
const label = (pkg, symbol) => `${PACKAGES[pkg]}.${symbol}`;

// The installed package root for one shim package, plus the manifest governing
// that release (exact, or the nearest older default, recorded in `release`).
function localRelease(pkg) {
  const agentDir = globalThis[configKey] || process.env.OCTET_PI_AGENT_DIR;
  const packageName = `@earendil-works/${PACKAGES[pkg]}`;
  for (const base of require.resolve.paths(packageName) ?? []) {
    try {
      const directory = realpathSync(join(base, packageName));
      const manifest = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'));
      const release = manifest.name === packageName && piRelease(manifest.version);
      if (release) return { root: directory, version: manifest.version, release };
    } catch {}
  }
  const install = locateInstalledPi(agentDir ? { ...process.env, OCTET_PI_AGENT_DIR: agentDir } : process.env);
  return { root: install.packages[pkg].dir, version: install.version, release: install.release };
}

// A reviewed individual Pi dist module, loaded from the supported installed
// package. The resolved file must stay inside that package.
function loadModule({ root, version, release }, pkg, path) {
  const base = release?.manifest.routeBase ?? 'dist';
  let file;
  try { file = realpathSync(join(root, base, path)); }
  catch (error) { throw absent(`${PACKAGES[pkg]}/${base}/${path} is missing in Pi ${version} (${error.code || error.message})`); }
  const suffix = relative(root, file);
  if (suffix.startsWith('..') || isAbsolute(suffix)) unsupported('Pi local helper', `module resolves outside the managed package in Pi ${version}`);
  if (!statSync(file).isFile()) throw absent(`${PACKAGES[pkg]}/${base}/${path} is not a file in Pi ${version}`);
  return require(file);
}

/** A reviewed Pi dist module by path, for the installed-Pi route. */
export function localModule(pkg, path) { return loadModule(localRelease(pkg), pkg, path); }

// Resolves one manifest-classified symbol to its value on the installed release.
function target(pkg, symbol, fallback) {
  let release;
  try { release = localRelease(pkg); }
  catch (error) {
    if (fallback !== undefined && error?.code === INSTALLED_PI_UNAVAILABLE) return fallback;
    throw error;
  }
  const { version } = release;
  const manifestVersion = release.release.manifest.pi;
  const entry = release.release.manifest.symbols[pkg]?.[symbol];
  if (!entry) {
    throw unsupported(label(pkg, symbol),
      `the adapter manifest ${manifestVersion}.json does not classify this symbol; refusing to bind it as undefined (Pi ${version})`);
  }
  if (entry.surface !== 'inert') {
    throw unsupported(label(pkg, symbol),
      entry.reason ?? `symbol is authority surface on Pi ${version}; octet keeps it host-owned`);
  }
  if (!entry.route) {
    if (fallback !== undefined) return fallback;
    throw unsupported(label(pkg, symbol),
      `Pi ${version} does not ship it${entry.removed ? ` (removed in ${entry.removed})` : ''}${entry.note ? `: ${entry.note}` : ''}`);
  }
  const module = loadModule(release, pkg, entry.route);
  if (!(symbol in module)) {
    throw absent(`${label(pkg, symbol)}: Pi ${version} routes it to ${release.release.manifest.routeBase}/${entry.route}, which does not export it`);
  }
  return module[symbol];
}

/** An inert helper function, resolved from the installed Pi release at call time. */
export function helper(pkg, symbol, fallback) {
  return (...args) => target(pkg, symbol, fallback)(...args);
}

/** A subclassable inert helper class, resolved from the installed release. */
export function helperClass(pkg, symbol, fallback) {
  const proxy = new Proxy(function () {}, {
    construct(_target, args, newTarget) {
      const Class = target(pkg, symbol, fallback);
      return Reflect.construct(Class, args, newTarget === proxy ? Class : newTarget);
    },
    get(_target, key) { return target(pkg, symbol, fallback)[key]; },
    apply(_target, receiver, args) { return Reflect.apply(target(pkg, symbol, fallback), receiver, args); },
  });
  return proxy;
}

export const refused = name => function () { unsupported(name, 'operation is not available through this host-owned shim'); };

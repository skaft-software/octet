import { createRequire } from 'node:module';
import { join, relative, isAbsolute } from 'node:path';
import { readFileSync, realpathSync } from 'node:fs';
import { locateInstalledPi, PACKAGES } from '../lib/installed-pi.mjs';
import { piReleaseSupported } from '../lib/pi-modules.mjs';
import { unsupported } from '../lib/errors.mjs';

const require = createRequire(import.meta.url);
const configKey = Symbol.for('octet.pi-compat.local-pi-dir');
export function configureLocalPi(directory) { globalThis[configKey] = directory; }
export function localModule(pkg, path) {
  const agentDir = globalThis[configKey] || process.env.OCTET_PI_AGENT_DIR;
  let root;
  const packageName = `@earendil-works/${PACKAGES[pkg]}`;
  for (const base of require.resolve.paths(packageName) ?? []) {
    try {
      const directory = realpathSync(join(base, packageName));
      const manifest = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'));
      if (manifest.name === packageName && piReleaseSupported(manifest.version)) { root = directory; break; }
    } catch {}
  }
  root ??= locateInstalledPi(agentDir ? { ...process.env, OCTET_PI_AGENT_DIR: agentDir } : process.env).packages[pkg].dir;
  const file = realpathSync(join(root, 'dist', path));
  const suffix = relative(root, file);
  if (suffix.startsWith('..') || isAbsolute(suffix)) unsupported('Pi local helper', 'module resolves outside the managed package');
  return require(file);
}
export function localFunction(pkg, path, name) {
  return (...args) => localModule(pkg, path)[name](...args);
}
export function localClass(pkg, path, name) {
  const target = function () {};
  const proxy = new Proxy(target, {
    construct(_target, args, newTarget) {
      const Class = localModule(pkg, path)[name];
      return Reflect.construct(Class, args, newTarget === proxy ? Class : newTarget);
    },
    get(_target, key) { return localModule(pkg, path)[name][key]; },
    apply(_target, receiver, args) { return Reflect.apply(localModule(pkg, path)[name], receiver, args); },
  });
  return proxy;
}
export const refused = name => function () { unsupported(name, 'operation is not available through this host-owned shim'); };

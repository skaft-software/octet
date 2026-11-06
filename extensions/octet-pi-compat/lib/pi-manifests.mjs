// Per-Pi-release capability manifests, and the only source of truth for how the
// shims describe and bind a Pi release.
//
// A manifest is small on purpose. It is not a copy of Pi's export list; it
// records (a) the route map: the specifier Pi's extension loader resolves and
// the deep module in the installed package that answers it, and (b) the
// classification table: for every symbol the shims can reach, whether it is
// inert (a pure helper) or authority (agent loops, sessions, tool execution,
// providers, terminal ownership), and which deep module serves it. Adding a Pi
// release is adding a manifest; there is no version string to edit.
//
// The resolution rule (`piRelease`):
//   - an exact manifest for the version wins;
//   - a version newer than the highest manifest reuses the nearest older
//     manifest, and reports the substitution in `substitutedFrom` so callers
//     diagnose an untested release instead of pretending it was tested;
//   - a version below the oldest manifest, or in another major series, has no
//     manifest that can stand in for it and is unsupported.
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { unsupported } from './errors.mjs';

const MANIFEST_DIR = new URL('../shims/manifests/', import.meta.url);
const VERSION_FILE = /^(\d+)\.(\d+)\.(\d+)\.json$/;

const compare = (a, b) => a[0] - b[0] || a[1] - b[1] || a[2] - b[2];
export const manifestFile = version => fileURLToPath(new URL(`${version}.json`, MANIFEST_DIR));

/** Every Pi release this build ships a manifest for, oldest first. */
export const PI_MANIFEST_VERSIONS = readdirSync(MANIFEST_DIR)
  .map(name => VERSION_FILE.exec(name))
  .filter(Boolean)
  .map(match => [Number(match[1]), Number(match[2]), Number(match[3])])
  .sort(compare)
  .map(parts => parts.join('.'));

const loaded = new Map();

/** The manifest shipped for exactly this Pi version, or null. */
export function piManifest(version) {
  if (!PI_MANIFEST_VERSIONS.includes(version)) return null;
  if (!loaded.has(version)) {
    const manifest = JSON.parse(readFileSync(manifestFile(version), 'utf8'));
    if (manifest.pi !== version || manifest.schema !== 1) {
      throw unsupported('pi manifest', `${manifestFile(version)} does not describe Pi ${version} with schema 1`);
    }
    loaded.set(version, manifest);
  }
  return loaded.get(version);
}

/**
 * The manifest that governs an installed Pi version, plus whether it is exact.
 * Versions below the oldest manifest, in another major series, or not `x.y.z`
 * resolve to null.
 */
export function piRelease(version) {
  // No leading zeros: `01.4.0` is not a Pi version.
  const match = typeof version === 'string' ? /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.exec(version) : null;
  if (!match) return null;
  const exact = piManifest(version);
  if (exact) return { manifest: exact, exact: true };
  const parts = [Number(match[1]), Number(match[2]), Number(match[3])];
  const older = PI_MANIFEST_VERSIONS
    .map(candidate => candidate.split('.').map(Number))
    .filter(candidate => compare(candidate, parts) < 0 && candidate[0] === parts[0])
    .sort(compare);
  if (!older.length) return null;
  const manifest = piManifest(older[older.length - 1].join('.'));
  return { manifest, exact: false, substitutedFrom: manifest.pi };
}

export function piReleaseSupported(version) { return piRelease(version) !== null; }

/** Whether a release is one of the manifests this build tested (not substituted). */
export const piReleaseTested = version => piRelease(version)?.exact === true;

/**
 * The supported-version statement derived from the shipped manifests: from the
 * oldest manifest to the end of its major series.
 */
export function piSupportedRange() {
  if (!PI_MANIFEST_VERSIONS.length) throw unsupported('pi manifests', 'no per-Pi-release manifest is shipped with this adapter');
  const major = Number(PI_MANIFEST_VERSIONS[PI_MANIFEST_VERSIONS.length - 1].split('.')[0]);
  return `>=${PI_MANIFEST_VERSIONS[0]} <${major + 1}.0.0`;
}

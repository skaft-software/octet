import { accessSync, constants, mkdirSync, readFileSync, renameSync, statSync, unlinkSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import module from 'node:module';

// Startup caches for this adapter, in one private directory.
//
// Two caches matter and both are only useful when they persist across
// processes: jiti's content-hashed transform cache (see `jitiCacheOptions`)
// and Node's V8 module compile cache. jiti's default (`node_modules/.cache`,
// else a shared `TMPDIR/jiti`) may not be writable in a packaged install and
// is not private, so the adapter owns an explicit owner-only directory:
//
//   1. `OCTET_PI_COMPAT_CACHE` — explicit override, also used by tests;
//   2. `<OCTET_EXTENSION_DIR>/.cache` — the reviewed extension directory, so
//      the cache follows the install and survives a fresh HOME (the review
//      capture warms this exact directory);
//   3. `~/.cache/octet-pi-compat` — a standalone run without an extension dir.
//
// A directory that cannot be created or is not writable falls through to the
// next choice, and the last resort is jiti's own default.
export function startupCacheDirectory() {
  const explicit = process.env.OCTET_PI_COMPAT_CACHE;
  if (explicit) return ensure(explicit);
  const extensionDir = process.env.OCTET_EXTENSION_DIR;
  if (extensionDir) {
    const local = ensure(join(extensionDir, '.cache'));
    if (local) return local;
  }
  try {
    const base = process.env.XDG_CACHE_HOME || join(homedir(), '.cache');
    return ensure(join(base, 'octet-pi-compat'));
  } catch {
    return undefined;
  }
}

function ensure(directory) {
  try {
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    accessSync(directory, constants.W_OK | constants.X_OK);
    return directory;
  } catch {
    return undefined;
  }
}

/// Enables Node's module compile cache for every module compiled from now on.
///
/// Best effort by contract: an
/// unavailable or unwritable cache only means the process starts a little
/// slower, never that startup fails.
export function enableModuleCompileCache() {
  const directory = startupCacheDirectory();
  if (!directory || process.env.NODE_COMPILE_CACHE) return directory;
  try {
    module.enableCompileCache?.(join(directory, 'node-compile-cache'));
  } catch {
    // Best effort only.
  }
  return directory;
}

/// Options pinning jiti's transform cache to the private directory.
///
/// Cache keys stay jiti's own (content hash of the transformed source and its
/// dependency closure); only the location changes, from a possibly unwritable
/// package directory or a shared `TMPDIR/jiti` to one owner-only directory.
export function jitiCacheOptions() {
  const directory = startupCacheDirectory();
  return directory ? { fsCache: join(directory, 'jiti') } : {};
}

/// Reads one persisted JSON document from the startup cache directory.
///
/// Persisted maps are optimizations: a missing, oversized, unreadable or
/// wrong-schema file is `undefined` and the caller recomputes.
export function readPersisted(name, schema, maxBytes = 16 * 1024 * 1024) {
  try {
    const directory = startupCacheDirectory();
    if (!directory) return undefined;
    const file = join(directory, name);
    if (statSync(file).size > maxBytes) return undefined;
    const value = JSON.parse(readFileSync(file, 'utf8'));
    return value?.schema === schema ? value : undefined;
  } catch { return undefined; }
}

/// Atomically replaces one persisted document (private mode, rename). Best effort.
export function writePersisted(name, value, maxBytes = 16 * 1024 * 1024) {
  try {
    const directory = startupCacheDirectory();
    if (!directory) return false;
    const text = JSON.stringify(value);
    if (Buffer.byteLength(text) > maxBytes) return false;
    const file = join(directory, name), temporary = `${file}.${process.pid}.tmp`;
    try {
      writeFileSync(temporary, text, { mode: 0o600 });
      renameSync(temporary, file);
    } catch (error) {
      // The name is per process, so a leftover from a full disk would never be reused.
      try { unlinkSync(temporary); } catch { /* nothing was written */ }
      throw error;
    }
    return true;
  } catch { return false; }
}

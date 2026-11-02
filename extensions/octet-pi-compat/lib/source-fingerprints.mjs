import { readPersisted, writePersisted } from './startup-cache.mjs';

// Persisted per-file stat fingerprints for the reviewed-source walk.
//
// A reviewed file's digest (and its relative-import specifiers) are reused only
// while its fingerprint (dev, ino, size, mtime ns, ctime ns) is unchanged. ctime
// cannot be restored by a writer, so a same-size rewrite with a restored mtime
// still changes the fingerprint. A file whose ctime is within RACY_NS of now
// when recorded is never recorded: a write inside the filesystem's timestamp
// granularity could otherwise leave an identical fingerprint (the "racy git"
// case). Any miss, mismatch or unreadable cache simply re-reads and re-hashes.
const SCHEMA = 'octet.pi-source-fingerprints/v1';
const FILE = 'source-fingerprints-v1.json';
const RACY_NS = 3_000_000_000n;
const MAX_ENTRIES = 20000;

export function fingerprint(stat) {
  return `${stat.dev}:${stat.ino}:${stat.size}:${stat.mtimeNs}:${stat.ctimeNs}`;
}

export class SourceFingerprints {
  constructor() {
    this.files = new Map();
    this.dirty = false;
    const stored = readPersisted(FILE, SCHEMA);
    if (stored && stored.files && typeof stored.files === 'object') {
      for (const [file, entry] of Object.entries(stored.files)) {
        if (typeof entry?.f === 'string' && /^[a-f0-9]{64}$/.test(entry.h) && Array.isArray(entry.i) && entry.i.every(item => typeof item === 'string')) this.files.set(file, entry);
      }
    }
  }
  /// `{ hash, imports }` when `stat` (bigint) matches the recorded fingerprint.
  lookup(file, stat) {
    const entry = this.files.get(file);
    return entry && entry.f === fingerprint(stat) ? { hash: entry.h, imports: entry.i } : undefined;
  }
  record(file, stat, hash, imports) {
    const now = BigInt(Date.now()) * 1_000_000n;
    if (now - stat.ctimeNs < RACY_NS || now - stat.mtimeNs < RACY_NS) return;
    const f = fingerprint(stat), previous = this.files.get(file);
    if (previous?.f === f && previous.h === hash) return;
    this.files.set(file, { f, h: hash, i: imports });
    this.dirty = true;
  }
  flush() {
    if (!this.dirty) return;
    this.dirty = false;
    if (this.files.size > MAX_ENTRIES) return;
    writePersisted(FILE, { schema: SCHEMA, files: Object.fromEntries(this.files) });
  }
}

let shared;
/// The process-wide cache (one load of the persisted file per process).
export function sharedSourceFingerprints() { return shared ??= new SourceFingerprints(); }

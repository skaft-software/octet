// Client for the host's negotiated bulk-object protocol (`bulk_objects_v1`).
//
// The host transfers an immutable blob by copying it into a per-extension
// transfer directory and returning a lease plus a single basename. Nothing in
// this module accepts a caller-supplied path: the locator must be one plain
// file name inside the directory the host negotiated, opened without following
// symlinks, with the declared size and SHA-256 verified before the bytes are
// used. A lease is always released, including on failure and cancellation.
import { createHash } from 'node:crypto';
import { closeSync, constants, fstatSync, openSync, readSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { bounded, invalid, rpcError, unsupported } from './errors.mjs';

export const BULK_PROFILE = 'local-file.v1';
export const MAX_BLOB_BYTES = 64 * 1024 * 1024;
const MAX_LOCATOR_BYTES = 256;

const sha256 = buffer => createHash('sha256').update(buffer).digest('hex');

/// Whether the host offered an absolute path on this platform's terms.
///
/// The *original* string is judged, not `resolve(relative)`: resolving a
/// relative path silently makes it absolute and would accept a relative offer.
/// POSIX absolute, Windows drive-absolute and Windows UNC are each recognised
/// explicitly so a Windows host path is not mangled by POSIX rules (or vice
/// versa).
function isAbsoluteOffer(value) {
  if (typeof value !== 'string' || value.length === 0) return false;
  if (value.startsWith('/')) return true;
  if (/^[A-Za-z]:[\\/]/.test(value)) return true;
  return /^\\\\[^\\/]+[\\/][^\\/]+/.test(value);
}

/// Validate the `bulk_objects_v1` object the host sends during negotiation.
export function negotiateBulk(protocol) {
  const offer = protocol?.bulk_objects_v1;
  if (offer === undefined || offer === null) return null;
  if (typeof offer !== 'object' || Array.isArray(offer)) invalid('bulk negotiation');
  if (offer.profile !== BULK_PROFILE) unsupported('bulk profile', 'only local-file.v1 is supported');
  bounded(offer.transfer_directory, 'bulk transfer directory', 4096);
  if (!isAbsoluteOffer(offer.transfer_directory)) invalid('bulk transfer directory');
  const limits = offer.limits ?? {};
  const objectBytes = limits.object_bytes ?? MAX_BLOB_BYTES;
  if (!Number.isSafeInteger(objectBytes) || objectBytes <= 0 || objectBytes > MAX_BLOB_BYTES) invalid('bulk object limit');
  return Object.freeze({ transferDirectory: resolve(offer.transfer_directory), objectBytes });
}

/// Validate one `$blob` reference without trusting any field of it.
export function validateBlobRef(value, objectBytes) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) invalid('blob reference');
  const keys = Object.keys(value).sort();
  if (keys.join(',') !== '$blob,bytes,digest,media_type') invalid('blob reference fields');
  bounded(value.$blob, 'blob identity', 512);
  if (!value.$blob) invalid('blob identity');
  if (!Number.isSafeInteger(value.bytes) || value.bytes < 0 || value.bytes > objectBytes) invalid('blob size');
  if (!value.digest || typeof value.digest !== 'object') invalid('blob digest');
  if (value.digest.algorithm !== 'sha256' || !/^[0-9a-f]{64}$/.test(value.digest.value ?? '')) invalid('blob digest');
  bounded(value.media_type, 'blob media type', 256);
  return value;
}

/// One basename inside the negotiated directory, never a caller path.
function resolveLocator(transferDirectory, locator) {
  bounded(locator, 'blob locator', MAX_LOCATOR_BYTES);
  if (!locator || locator.includes('/') || locator.includes('\\') || locator === '.' || locator === '..') invalid('blob locator');
  const path = join(transferDirectory, locator);
  if (resolve(path) !== join(transferDirectory, locator)) invalid('blob locator');
  return path;
}

function readExactly(path, expectedBytes) {
  const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stats = fstatSync(fd);
    if (!stats.isFile()) throw rpcError(-32032, 'blob_unavailable transfer is not a regular file');
    if (stats.size !== expectedBytes) throw rpcError(-32032, 'blob_unavailable transfer size mismatch');
    const buffer = Buffer.allocUnsafe(expectedBytes);
    let offset = 0;
    while (offset < expectedBytes) {
      const read = readSync(fd, buffer, offset, expectedBytes - offset, offset);
      if (read <= 0) throw rpcError(-32032, 'blob_unavailable transfer ended early');
      offset += read;
    }
    return buffer;
  } finally {
    closeSync(fd);
  }
}

/// Read one host-provided blob. `request` sends an extension→host call.
export function createBulkReader({ negotiation, request }) {
  if (!negotiation) unsupported('bulk_objects_v1', 'the host did not negotiate bulk objects');
  return Object.freeze({
    async read(parentRequestId, reference, { signal } = {}) {
      if (!Number.isSafeInteger(parentRequestId) || parentRequestId < 0) invalid('bulk parent request id');
      if (signal?.aborted) throw rpcError(-32800, 'blob read cancelled');
      const ref = validateBlobRef(reference, negotiation.objectBytes);
      let lease;
      let cancelled = false;
      // One lease is released once, whether it is released inline or by the
      // late-reply handler after cancellation.
      const released = new Set();
      const releaseOnce = id => {
        if (!id || released.has(id)) return Promise.resolve();
        released.add(id);
        return Promise.resolve(request('bulk/release', { parent_request_id: parentRequestId, id })).catch(() => {});
      };
      const onAbort = () => { cancelled = true; };
      signal?.addEventListener('abort', onAbort, { once: true });
      const pending = request('bulk/read', { parent_request_id: parentRequestId, profile: BULK_PROFILE, blob: ref });
      // A reply that lands after cancellation still owns a lease: release it
      // rather than leaking it, and never publish the content as success.
      pending.then(reply => {
        if (cancelled && reply?.lease) void releaseOnce(reply.lease);
      }, () => {});
      try {
        let abortListener;
        const abort = new Promise((_, reject) => {
          abortListener = () => reject(rpcError(-32800, 'blob read cancelled'));
          if (signal?.aborted) reject(rpcError(-32800, 'blob read cancelled'));
          else signal?.addEventListener('abort', abortListener, { once: true });
        });
        try {
          lease = await Promise.race([pending, abort]);
        } finally {
          // The race may settle on the reply; an unremoved listener would
          // accumulate on a caller that reuses one signal across reads.
          if (abortListener) signal?.removeEventListener('abort', abortListener);
        }
        if (cancelled) throw rpcError(-32800, 'blob read cancelled');
        if (!lease || typeof lease !== 'object') invalid('blob lease');
        if (lease.profile !== BULK_PROFILE) unsupported('bulk profile', 'the lease names another transfer profile');
        bounded(lease.lease, 'blob lease', 512);
        if (!lease.lease) invalid('blob lease');
        if (lease.bytes !== ref.bytes) invalid('blob lease size');
        const path = resolveLocator(negotiation.transferDirectory, lease.locator);
        if (signal?.aborted) throw rpcError(-32800, 'blob read cancelled');
        const bytes = readExactly(path, ref.bytes);
        if (sha256(bytes) !== ref.digest.value) throw rpcError(-32032, 'blob_unavailable digest mismatch');
        return bytes;
      } finally {
        signal?.removeEventListener('abort', onAbort);
        if (lease?.lease) await releaseOnce(lease.lease);
      }
    },
  });
}

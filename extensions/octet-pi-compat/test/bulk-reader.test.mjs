// Reader for the host's negotiated `bulk_objects_v1` protocol.
//
// These tests drive the reader against a real temporary transfer directory and a
// scripted host call, so the file-system safety, digest/size verification and
// lease release are exercised for real. Checks that need the live host are named
// at the bottom of this file instead of being faked here.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { getEventListeners } from 'node:events';
import { mkdtempSync, rmSync, symlinkSync, writeFileSync, chmodSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createBulkReader, negotiateBulk, BULK_PROFILE } from '../lib/bulk.mjs';

const digest = buffer => createHash('sha256').update(buffer).digest('hex');
const content = Buffer.from('host staged payload for the pi bridge');

function fixture(t) {
  const transfer = mkdtempSync(join(tmpdir(), 'octet-bulk-transfer-'));
  t.after(() => rmSync(transfer, { recursive: true, force: true }));
  const calls = [];
  const host = (leases) => async (method, params) => {
    calls.push({ method, params });
    if (method === 'bulk/read') return leases.read ?? {
      lease: 'l'.repeat(32), profile: BULK_PROFILE, locator: 'blob-1', bytes: content.length,
    };
    if (method === 'bulk/release') return { released: true };
    throw new Error(`unexpected ${method}`);
  };
  const negotiation = negotiateBulk({ bulk_objects_v1: { profile: BULK_PROFILE, transfer_directory: transfer, limits: { object_bytes: 1024 } } });
  return { transfer, calls, host, negotiation };
}

const reference = (overrides = {}) => ({
  $blob: 'b'.repeat(32), bytes: content.length, digest: { algorithm: 'sha256', value: digest(content) }, media_type: 'text/plain', ...overrides,
});

test('a negotiated blob is read, verified and its lease released', async t => {
  const { transfer, calls, host, negotiation } = fixture(t);
  writeFileSync(join(transfer, 'blob-1'), content);
  const reader = createBulkReader({ negotiation, request: host({}) });
  const bytes = await reader.read(7, reference());
  assert.deepEqual(bytes, content);
  assert.equal(calls.filter(call => call.method === 'bulk/read').length, 1);
  assert.deepEqual(calls.find(call => call.method === 'bulk/read').params, {
    parent_request_id: 7, profile: BULK_PROFILE, blob: reference(),
  });
  assert.deepEqual(calls.filter(call => call.method === 'bulk/release').map(call => call.params.id), ['l'.repeat(32)]);
});

test('malformed references are refused before any host call', async t => {
  const { calls, host, negotiation } = fixture(t);
  const reader = createBulkReader({ negotiation, request: host({}) });
  const cases = [
    null,
    {},
    { ...reference(), extra: 1 },
    reference({ $blob: '' }),
    reference({ bytes: -1 }),
    reference({ bytes: 4096 }),
    reference({ digest: { algorithm: 'sha256', value: 'ZZ' } }),
    reference({ digest: { algorithm: 'sha512', value: digest(content) } }),
    reference({ media_type: 7 }),
  ];
  for (const value of cases) await assert.rejects(reader.read(1, value), undefined, JSON.stringify(value));
  assert.equal(calls.length, 0, 'a malformed reference never reaches the host');
});

test('locators are basenames inside the negotiated directory only', async t => {
  for (const locator of ['../escape', '/etc/passwd', 'nested/blob', '..', '', 'a\\b']) {
    const { transfer, calls, host, negotiation } = fixture(t);
    writeFileSync(join(transfer, 'blob-1'), content);
    const reader = createBulkReader({ negotiation, request: async (method, params) => {
      calls.push({ method, params });
      if (method === 'bulk/read') return { lease: 'x', profile: BULK_PROFILE, locator, bytes: content.length };
      return { released: true };
    } });
    await assert.rejects(reader.read(1, reference()), undefined, locator);
    assert.equal(calls.filter(call => call.method === 'bulk/release').length, 1, 'the lease is still released');
  }
});

test('a symlinked transfer is refused rather than followed', async t => {
  const { transfer, host, negotiation } = fixture(t);
  const outside = join(mkdtempSync(join(tmpdir(), 'octet-bulk-outside-')), 'secret');
  writeFileSync(outside, content);
  symlinkSync(outside, join(transfer, 'blob-1'));
  const reader = createBulkReader({ negotiation, request: host({}) });
  await assert.rejects(reader.read(1, reference()));
});

test('digest and size mismatches are refused and the lease released', async t => {
  const { transfer, calls, host, negotiation } = fixture(t);
  const sameLengthDifferent = Buffer.from(content);
  sameLengthDifferent[0] ^= 0xff; // identical length, different bytes
  writeFileSync(join(transfer, 'blob-1'), sameLengthDifferent);
  const reader = createBulkReader({ negotiation, request: host({}) });
  await assert.rejects(reader.read(1, reference()), /digest mismatch/);
  assert.equal(calls.filter(call => call.method === 'bulk/release').length, 1);

  const short = fixture(t);
  writeFileSync(join(short.transfer, 'blob-1'), Buffer.from('short'));
  const shortReader = createBulkReader({ negotiation: short.negotiation, request: short.host({}) });
  await assert.rejects(shortReader.read(1, reference()), /size mismatch/);
  assert.equal(short.calls.filter(call => call.method === 'bulk/release').length, 1);
});

test('a foreign or retired blob surfaces as a host refusal and releases nothing', async t => {
  const { calls, host, negotiation } = fixture(t);
  const refusing = async (method, params) => {
    calls.push({ method, params });
    const error = new Error('blob_unavailable');
    error.code = -32032;
    throw error;
  };
  const reader = createBulkReader({ negotiation, request: refusing });
  await assert.rejects(reader.read(1, reference()), /blob_unavailable/);
  assert.equal(calls.filter(call => call.method === 'bulk/release').length, 0);
});

test('cancellation stops before the host call and after the read', async t => {
  const { transfer, calls, host, negotiation } = fixture(t);
  writeFileSync(join(transfer, 'blob-1'), content);
  const reader = createBulkReader({ negotiation, request: host({}) });

  const aborted = new AbortController();
  aborted.abort();
  await assert.rejects(reader.read(1, reference(), { signal: aborted.signal }), /cancelled/);
  assert.equal(calls.length, 0, 'a cancelled read never asks the host');

  const mid = new AbortController();
  const cancelling = async (method, params) => {
    calls.push({ method, params });
    if (method === 'bulk/read') { mid.abort(); return { lease: 'l2', profile: BULK_PROFILE, locator: 'blob-1', bytes: content.length }; }
    return { released: true };
  };
  const cancelReader = createBulkReader({ negotiation, request: cancelling });
  await assert.rejects(cancelReader.read(1, reference(), { signal: mid.signal }), /cancelled/);
  assert.deepEqual(calls.filter(call => call.method === 'bulk/release').map(call => call.params.id), ['l2']);
});

test('an old host that never negotiated bulk objects is an explicit unsupported error', async t => {
  assert.equal(negotiateBulk({}), null);
  assert.equal(negotiateBulk(undefined), null);
  // Construction fails fast: a bridge that cannot read blobs must not pretend it can.
  assert.throws(
    () => createBulkReader({ negotiation: null, request: async () => { throw new Error('must not be called'); } }),
    /bulk_objects_v1/,
  );
});

test('negotiation validates the offered path as written, per platform', async () => {
  const offer = dir => () => negotiateBulk({ bulk_objects_v1: { profile: BULK_PROFILE, transfer_directory: dir, limits: { object_bytes: 16 } } });
  for (const dir of ['/tmp/transfer', '/', 'C:\\transfer', 'C:/transfer', '\\\\server\\share\\transfer']) {
    assert.ok(offer(dir)(), `absolute offer accepted: ${dir}`);
  }
  for (const dir of ['relative/dir', 'transfer', '', '.', '../up']) {
    assert.throws(offer(dir), /bulk transfer directory/, `relative offer refused: ${dir}`);
  }
  assert.throws(() => negotiateBulk({ bulk_objects_v1: { profile: 'other.v1', transfer_directory: '/tmp/x', limits: { object_bytes: 16 } } }), /bulk profile/);
  assert.throws(() => negotiateBulk({ bulk_objects_v1: { profile: BULK_PROFILE, transfer_directory: '/tmp/x', limits: { object_bytes: 0 } } }), /bulk object limit/);
});

test('cancellation while bulk/read is pending leaks no lease and never succeeds', async t => {
  const { transfer, negotiation } = fixture(t);
  writeFileSync(join(transfer, 'blob-1'), content);
  const calls = [];
  let deliver;
  const deferred = new Promise(resolve => { deliver = resolve; });
  const request = async (method, params) => {
    calls.push({ method, params });
    if (method === 'bulk/read') return deferred;
    return { released: true };
  };
  const reader = createBulkReader({ negotiation, request });
  const controller = new AbortController();
  const read = reader.read(1, reference(), { signal: controller.signal });
  await new Promise(resolve => setTimeout(resolve, 10));
  controller.abort();
  await assert.rejects(read, /cancelled/);
  // The reply arrives after cancellation: its lease must still be released and
  // the content must not be published as a successful read.
  deliver({ lease: 'late-lease', profile: BULK_PROFILE, locator: 'blob-1', bytes: content.length });
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.deepEqual(calls.filter(call => call.method === 'bulk/release').map(call => call.params.id), ['late-lease']);
});

test('repeated reads on one signal leave no abort listeners behind', async t => {
  const { transfer, host, negotiation } = fixture(t);
  writeFileSync(join(transfer, 'blob-1'), content);
  const reader = createBulkReader({ negotiation, request: host({}) });
  const controller = new AbortController();
  for (let i = 0; i < 25; i++) await reader.read(1, reference(), { signal: controller.signal });
  const leftover = getEventListeners(controller.signal, 'abort').length;
  assert.equal(leftover, 0, `one reused signal must not accumulate listeners (${leftover})`);
  controller.abort();
});

// Requires the live host (not mocked here):
//  - that the host writes the blob and returns a lease for a *foreign or retired*
//    owner with `blob_unavailable` given a real connection generation;
//  - that the negotiated transfer_directory really is that connection's store and
//    that the host refuses a locator it did not issue;
//  - that `bulk/release` after cancellation is observed by a real store sweep.
